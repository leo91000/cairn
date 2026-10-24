//! Shared host budgets. VM address spaces are ceilings, never reservations.
use crate::{
    error::{Error, Result},
    nodes::Resources,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

pub const FILE: &str = "node-budget.json";
const CONTROLLER_MEMORY_MIB: u64 = 512;
const MIN_GUEST_MEMORY_MIB: u64 = 128;
/// Reclaim starts one tenth below the shared RAM limit.
const RECLAIM_HEADROOM_DIVISOR: u64 = 10;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub slots: usize,
    pub limits: Resources,
}

impl Budget {
    pub fn vm_memory_mib(&self) -> u64 {
        self.limits
            .memory_mi_b
            .saturating_sub(CONTROLLER_MEMORY_MIB)
    }

    pub fn validate(&self) -> Result<()> {
        self.limits.validate()?;
        if self.vm_memory_mib() < MIN_GUEST_MEMORY_MIB {
            return Err(Error::bad(
                "Shared RAM must allow 512 MiB for the controller and at least 128 MiB for a guest.",
            ));
        }
        if !(1..=4096).contains(&self.slots) {
            return Err(Error::bad("Use between 1 and 4096 execution slots."));
        }
        Ok(())
    }

    pub fn apply(&self, cgroup: &Path) -> Result<()> {
        self.validate()?;
        let memory = self.limits.memory_mi_b * 1_048_576;
        let previous = std::fs::read_to_string(cgroup.join("memory.max"))?;
        if previous
            .trim()
            .parse::<u64>()
            .ok()
            .is_none_or(|limit| memory < limit)
        {
            let current = memory_current(cgroup)?;
            let needed = current
                .saturating_add(CONTROLLER_MEMORY_MIB * 1_048_576)
                .saturating_sub(memory);
            if needed > 0 {
                // Image imports can fill the page cache. Reclaim file pages before
                // checking a decrease; never use a smaller limit to force an OOM.
                let _ = std::fs::write(
                    cgroup.join("memory.reclaim"),
                    format!("{needed} swappiness=0"),
                );
            }
            if memory_current(cgroup)?.saturating_add(CONTROLLER_MEMORY_MIB * 1_048_576) > memory {
                return Err(Error::conflict(
                    "Shared RAM budget is below current usage plus the controller reserve.",
                ));
            }
        }
        let old_cpu = std::fs::read(cgroup.join("cpu.max"))?;
        std::fs::write(
            cgroup.join("cpu.max"),
            format!("{} 100000", u64::from(self.limits.cpu) * 100_000),
        )?;
        if let Err(error) = std::fs::write(cgroup.join("memory.max"), memory.to_string()) {
            std::fs::write(cgroup.join("cpu.max"), old_cpu)?;
            return Err(error.into());
        }

        // Journal page cache shares this cgroup with the guests. At the hard
        // limit, GFP_NOFS journal allocations cannot reclaim it and the kernel
        // kills a VM or the controller; above memory.high it reclaims and
        // throttles instead.
        let high = memory - memory / RECLAIM_HEADROOM_DIVISOR;
        if let Err(error) = std::fs::write(cgroup.join("memory.high"), high.to_string()) {
            tracing::warn!(message = %error, "Could not set the shared memory reclaim threshold");
        }
        Ok(())
    }
}

fn memory_current(cgroup: &Path) -> Result<u64> {
    std::fs::read_to_string(cgroup.join("memory.current"))?
        .trim()
        .parse::<u64>()
        .map_err(Error::internal)
}

/// Inactive file pages are reclaimable; imported images must not consume slots.
pub fn memory_usage(cgroup: &Path) -> Result<u64> {
    let current = memory_current(cgroup)?;
    let inactive = std::fs::read_to_string(cgroup.join("memory.stat"))?
        .lines()
        .find_map(|line| {
            line.strip_prefix("inactive_file ")
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or(0);
    Ok(current.saturating_sub(inactive))
}

/// Mount only the controller's private cgroup namespace, never the host hierarchy.
pub async fn cgroup() -> Result<PathBuf> {
    let namespace = std::fs::read_to_string("/proc/self/cgroup")?;
    if !matches!(namespace.trim(), "0::/" | "0::/cairn-shared")
        || !Path::new("/sys/fs/cgroup/memory.max").exists()
    {
        return Err(Error::unavailable(
            "Shared budgets require a private cgroup-v2 controller container.",
        ));
    }
    let path = PathBuf::from("/run/cairn-cgroup");
    crate::skills::private_dir(&path).await?;
    super::host::command(
        "mount",
        &[
            "-t",
            "cgroup2",
            "-o",
            "rw,nosuid,nodev,noexec",
            "none",
            path.to_str().unwrap(),
        ],
    )
    .await?;
    // Namespace roots with nsdelegate only expose delegation controls. Put the
    // controller and its children in a leaf, then enable CPU/memory controllers.
    let shared = path.join("cairn-shared");
    std::fs::create_dir_all(&shared)?;
    // Docker exec/health checks can start while the controller is restarting.
    // Drain root processes again if one arrived between migration and enablement.
    for _ in 0..250 {
        for pid in std::fs::read_to_string(path.join("cgroup.procs"))?.lines() {
            if pid == "0" {
                continue;
            }
            if let Err(error) = std::fs::write(shared.join("cgroup.procs"), pid)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                return Err(error.into());
            }
        }
        match std::fs::write(path.join("cgroup.subtree_control"), "+cpu +memory") {
            Ok(()) => return Ok(shared),
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::unavailable(
        "Shared cgroup delegation is still busy.",
    ))
}

pub fn allocated(directory: &Path) -> io::Result<u64> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut bytes = 0u64;
    for entry in entries {
        let entry = entry?;
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.is_dir() {
            bytes = bytes.saturating_add(allocated(&entry.path())?);
        } else if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.blocks().saturating_mul(512));
        }
    }
    Ok(bytes)
}

struct DiskUsage {
    sampled: Instant,
    bytes: u64,
}

fn disk_usage() -> &'static Mutex<HashMap<PathBuf, DiskUsage>> {
    static USAGE: OnceLock<Mutex<HashMap<PathBuf, DiskUsage>>> = OnceLock::new();
    USAGE.get_or_init(Default::default)
}

/// Reconcile once a second; admitted writes debit conservative headroom in between.
pub fn disk_bytes(state: &Path) -> io::Result<u64> {
    let mut usage = disk_usage()
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))?;
    if let Some(value) = usage.get(state)
        && value.sampled.elapsed() < Duration::from_secs(1)
    {
        return Ok(value.bytes);
    }
    let mut bytes = 0_u64;
    if state.join("native-accounting.json").exists() {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join("native-accounting.json"))?)
                .map_err(io::Error::other)?;
        let data = value["data"]
            .as_str()
            .ok_or_else(|| io::Error::other("Missing native data root"))?;
        bytes =
            crate::native::disk::usage_sync(Path::new(data), state).map_err(io::Error::other)?;
    } else {
        for root in crate::storage::environment::ROOTS
            .into_iter()
            .chain(["images"])
        {
            bytes = bytes.saturating_add(allocated(&state.join(root))?);
        }
    }
    usage.insert(
        state.to_owned(),
        DiskUsage {
            sampled: Instant::now(),
            bytes,
        },
    );
    Ok(bytes)
}

pub fn charge_disk(state: &Path, bytes: u64) -> io::Result<()> {
    let mut usage = disk_usage()
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))?;
    if let Some(value) = usage.get_mut(state) {
        value.bytes = value.bytes.saturating_add(bytes);
    }
    Ok(())
}

/// Restrict filesystem headroom to the shared local cache/journal budget, if configured.
pub fn disk_space(path: &Path, total: u64, free: u64) -> io::Result<(u64, u64)> {
    for state in path.ancestors() {
        let file = state.join(FILE);
        if !file.exists() {
            continue;
        }
        let bytes = std::fs::read(file)?;
        let budget: Budget = if state.join("native-accounting.json").exists() {
            // Native controller records fence older binaries with a protocol
            // marker. The shared quota reader must validate that same format;
            // Firecracker and old readers retain their strict Budget contract.
            let value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            crate::native::native_budget(&value).map_err(io::Error::other)?
        } else {
            serde_json::from_slice(&bytes).map_err(io::Error::other)?
        };
        let limit = budget.limits.disk_mi_b * 1_048_576;
        let remaining = limit.saturating_sub(disk_bytes(state)?);
        let policy: crate::storage::policy::Policy =
            match std::fs::read(state.join("storage-policy.json")) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    crate::storage::policy::Policy::default()
                }
                Err(error) => return Err(error),
            };
        // Callers subtract the physical reserve from free space. Keep that
        // reserve outside the quota, even when the quota is smaller than it.
        return Ok((
            total,
            free.min(policy.reserve(total).saturating_add(remaining)),
        ));
    }
    Ok((total, free))
}

pub fn pressure(
    memory: u64,
    memory_limit: u64,
    free_disk: u64,
    disk_reserve: u64,
) -> Option<&'static str> {
    if memory.saturating_add(256) >= memory_limit {
        return Some("memory");
    }
    if free_disk <= disk_reserve.saturating_add(128 * 1_048_576) {
        return Some("disk");
    }
    None
}

/// Above this share of the shared RAM budget, running guests give memory back.
const RECLAIM_ABOVE_PERCENT: u64 = 85;
/// Below this share, guests get their pressure balloon back.
const RETURN_BELOW_PERCENT: u64 = 75;
/// Memory a guest keeps available under pressure, enough for a compiler or
/// test run. Squeezing further only moves its file cache to the host and
/// makes the guest thrash; the shared cgroup stays the hard limit.
const GUEST_FLOOR_MIB: u64 = 1024;
/// Largest balloon change per guest and monitor tick.
const BALLOON_STEP_MIB: u64 = 256;

/// The pressure balloon size a guest should move to, if any. Between the two
/// thresholds the balloon holds its size so guests do not oscillate.
pub fn balloon_target(
    used_mib: u64,
    limit_mib: u64,
    actual_mib: u64,
    available_mib: u64,
) -> Option<u64> {
    if used_mib * 100 > limit_mib * RECLAIM_ABOVE_PERCENT {
        let extra = available_mib
            .saturating_sub(GUEST_FLOOR_MIB)
            .min(BALLOON_STEP_MIB);
        return (extra > 0).then_some(actual_mib + extra);
    }
    if used_mib * 100 < limit_mib * RETURN_BELOW_PERCENT && actual_mib > 0 {
        return Some(actual_mib.saturating_sub(BALLOON_STEP_MIB));
    }
    None
}

/// Reclaims guest memory while the shared host budget is tight, and returns it
/// once the pressure is gone. Ballooning is cooperative; the cgroup remains
/// the hard host limit.
pub async fn rebalance(state: &Path, used_mib: u64, limit_mib: u64) -> Result<()> {
    let jails = state.join("jails/firecracker");
    let Ok(mut entries) = tokio::fs::read_dir(jails).await else {
        return Ok(());
    };
    while let Some(entry) = entries.next_entry().await? {
        let socket = entry.path().join("root/api.sock");
        if !socket.exists() {
            continue;
        }
        // Idle retention owns a VM's balloon from its statistics read until
        // the CPUs pause. A decision from statistics read here must not replace
        // the target that retention waits for the guest to acknowledge.
        let Ok(_owner) = crate::file_lock::exclusive_directory(&entry.path(), "Balloon is owned.")
        else {
            continue;
        };

        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .timeout(Duration::from_millis(500))
            .build()
            .map_err(Error::internal)?;
        let Ok(response) = client
            .get("http://localhost/balloon/statistics")
            .send()
            .await
        else {
            continue;
        };
        let Ok(stats) = response.json::<serde_json::Value>().await else {
            continue;
        };
        let actual = stats["actual_mib"].as_u64().unwrap_or(0);
        let available = stats["available_memory"].as_u64().unwrap_or(0) / 1_048_576;
        let Some(target) = balloon_target(used_mib, limit_mib, actual, available) else {
            continue;
        };

        // Never deflate a paused retained VM, nor a balloon whose earlier
        // target is still being reached.
        if target < actual {
            let inflating = stats["target_mib"].as_u64().unwrap_or(0) > actual + 16;
            let paused = match client.get("http://localhost/").send().await {
                Ok(response) => response
                    .json::<serde_json::Value>()
                    .await
                    .map_or(true, |vm| vm["state"] != "Running"),
                Err(_) => true,
            };
            if inflating || paused {
                continue;
            }
        }

        let _ = client
            .patch("http://localhost/balloon")
            .json(&serde_json::json!({ "amount_mib": target }))
            .send()
            .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_budget_counts_runtime_images() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("images/current");
        std::fs::create_dir_all(&image).unwrap();
        std::fs::write(image.join("root.ext4"), vec![7; 8192]).unwrap();

        assert_eq!(disk_bytes(root.path()).unwrap(), 8192);
    }

    #[test]
    fn guest_memory_leaves_controller_headroom_without_dividing_by_slots() {
        let mut budget = Budget {
            slots: 1,
            limits: Resources {
                cpu: 3,
                memory_mi_b: 7168,
                disk_mi_b: 32768,
            },
        };
        assert_eq!(budget.vm_memory_mib(), 6656);
        budget.slots = 16;
        assert_eq!(budget.vm_memory_mib(), 6656);
        budget.limits.memory_mi_b = 639;
        assert!(budget.validate().is_err());
        budget.limits.memory_mi_b = 640;
        budget.validate().unwrap();
        assert_eq!(budget.vm_memory_mib(), 128);
    }

    #[test]
    fn reconfiguration_never_kills_existing_work_and_cpu_is_shared() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("memory.current"),
            (512 * 1_048_576u64).to_string(),
        )
        .unwrap();
        std::fs::write(root.path().join("cpu.max"), "max 100000").unwrap();
        std::fs::write(root.path().join("memory.max"), "max").unwrap();
        let mut budget = Budget {
            slots: 12,
            limits: Resources {
                cpu: 8,
                memory_mi_b: 1024,
                disk_mi_b: 32768,
            },
        };
        budget.apply(root.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("cpu.max")).unwrap(),
            "800000 100000"
        );
        budget.limits.memory_mi_b = 768;
        assert_eq!(budget.apply(root.path()).unwrap_err().status, 409);
        assert_eq!(
            std::fs::read_to_string(root.path().join("memory.max")).unwrap(),
            (1024 * 1_048_576u64).to_string()
        );
        std::fs::write(
            root.path().join("memory.current"),
            (1024 * 1_048_576u64).to_string(),
        )
        .unwrap();
        budget.limits.memory_mi_b = 1024;
        budget.apply(root.path()).unwrap();
    }

    #[test]
    fn shared_memory_is_reclaimed_before_its_hard_limit_kills_vms() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("memory.current"), "0").unwrap();
        std::fs::write(root.path().join("cpu.max"), "max 100000").unwrap();
        std::fs::write(root.path().join("memory.max"), "max").unwrap();
        let budget = Budget {
            slots: 8,
            limits: Resources {
                cpu: 7,
                memory_mi_b: 36352,
                disk_mi_b: 32768,
            },
        };
        budget.apply(root.path()).unwrap();
        let max = 36352 * 1_048_576u64;
        let high = std::fs::read_to_string(root.path().join("memory.high"))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("memory.max")).unwrap(),
            max.to_string()
        );
        assert!(
            high < max,
            "page cache must be reclaimed before an OOM kill"
        );
        assert!(max - high >= 1024 * 1_048_576);
        assert!(high >= max - max / 8, "guests keep most of the budget");
    }

    #[test]
    fn pressure_balloons_leave_a_working_floor_and_deflate_once_pressure_falls() {
        let limit = 36352;
        let tight = limit * 90 / 100;
        let calm = limit * 70 / 100;
        let between = limit * 80 / 100;

        // Above the reclaim threshold, a guest gives back memory in steps...
        assert_eq!(balloon_target(tight, limit, 0, 8192), Some(256));
        // ...but keeps enough to run a compiler instead of thrashing.
        assert_eq!(balloon_target(tight, limit, 4096, 1100), Some(4096 + 76));
        assert_eq!(balloon_target(tight, limit, 4096, 1024), None);

        // Production 2026-10-04: a guest stayed squeezed to ~2 GiB of 34 GiB
        // hours after the peak. Once the node is calm, memory comes back.
        assert_eq!(
            balloon_target(calm, limit, 31_000, 2000),
            Some(31_000 - 256)
        );
        assert_eq!(balloon_target(calm, limit, 100, 2000), Some(0));
        assert_eq!(balloon_target(calm, limit, 0, 2000), None);

        // The band between thresholds holds the current size to avoid oscillation.
        assert_eq!(balloon_target(between, limit, 4096, 8192), None);
    }

    #[test]
    fn image_page_cache_is_reclaimable_but_guest_anonymous_memory_counts() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("memory.current"), "1073741824").unwrap();
        std::fs::write(
            root.path().join("memory.stat"),
            "anon 268435456\ninactive_file 805306368\n",
        )
        .unwrap();
        assert_eq!(memory_usage(root.path()).unwrap(), 268435456);
    }

    #[test]
    fn local_disk_budget_counts_every_conversation_without_following_links() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("keep"), vec![1; 1_048_576]).unwrap();
        let budget = Budget {
            slots: 12,
            limits: Resources {
                cpu: 8,
                memory_mi_b: 32768,
                disk_mi_b: 128,
            },
        };
        std::fs::write(root.path().join(FILE), serde_json::to_vec(&budget).unwrap()).unwrap();
        for (namespace, run) in [("disks", "first"), ("environments", "second")] {
            let disk = root.path().join(namespace).join(run);
            std::fs::create_dir_all(&disk).unwrap();
            std::fs::write(disk.join("journal"), vec![1; 1_048_576]).unwrap();
            std::os::unix::fs::symlink(external.path(), disk.join("external")).unwrap();
        }
        assert_eq!(disk_bytes(root.path()).unwrap(), 2 * 1_048_576);
        charge_disk(root.path(), 4 * 1_048_576).unwrap();
        let (total, free) = disk_space(
            &root.path().join("disks/first"),
            128 * 1024 * 1_048_576,
            64 * 1024 * 1_048_576,
        )
        .unwrap();
        assert_eq!(total, 128 * 1024 * 1_048_576);
        assert_eq!(
            free - crate::storage::policy::Policy::default().reserve(total),
            122 * 1_048_576
        );
    }

    #[test]
    fn disk_quota_preserves_physical_reserve_without_reserving_it_twice() {
        let root = tempfile::tempdir().unwrap();
        let budget = Budget {
            slots: 12,
            limits: Resources {
                cpu: 8,
                memory_mi_b: 32768,
                disk_mi_b: 5120,
            },
        };
        std::fs::write(root.path().join(FILE), serde_json::to_vec(&budget).unwrap()).unwrap();
        let total = 128 * 1024 * 1_048_576;
        let reserve = crate::storage::policy::Policy::default().reserve(total);
        let (_, free) = disk_space(root.path(), total, total / 2).unwrap();
        assert_eq!(free - reserve, 5120 * 1_048_576);
        assert_eq!(pressure(0, 32768, free, reserve), None);
        charge_disk(root.path(), 5120 * 1_048_576).unwrap();
        let (_, free) = disk_space(root.path(), total, total / 2).unwrap();
        assert_eq!(pressure(0, 32768, free, reserve), Some("disk"));
        let (_, free) = disk_space(root.path(), total, reserve - 1).unwrap();
        assert_eq!(free, reserve - 1);
    }

    #[test]
    fn shared_disk_quota_accepts_only_the_verified_native_budget_protocol() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut record = serde_json::json!({
            "slots": 2,
            "limits": { "cpu": 2, "memoryMiB": 1024, "diskMiB": 512 },
            "nativeModeProtocol": 1,
        });
        let path = root.path().join(FILE);
        let bytes = serde_json::to_vec(&record).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            serde_json::from_slice::<Budget>(&bytes).is_err(),
            "Older controller/VM decoding must remain fenced"
        );
        let total = 128 * 1024 * 1_048_576;
        assert!(
            disk_space(root.path(), total, total).is_err(),
            "A VM budget cannot accept a native marker"
        );
        std::fs::write(
            root.path().join("native-accounting.json"),
            serde_json::to_vec(&serde_json::json!({ "data": data.path() })).unwrap(),
        )
        .unwrap();
        let (_, free) = disk_space(root.path(), total, total).unwrap();
        let reserve = crate::storage::policy::Policy::default().reserve(total);
        assert!(free > reserve + 511 * 1_048_576 && free <= reserve + 512 * 1_048_576);
        charge_disk(root.path(), 512 * 1_048_576).unwrap();
        assert_eq!(disk_space(root.path(), total, total).unwrap().1, reserve);
        for protocol in [
            serde_json::json!(2),
            serde_json::json!(null),
            serde_json::json!("1"),
        ] {
            record["nativeModeProtocol"] = protocol;
            std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            assert!(disk_space(root.path(), total, total).is_err());
        }
        record.as_object_mut().unwrap().remove("nativeModeProtocol");
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(
            disk_space(root.path(), total, total).is_ok(),
            "Legacy protected native budgets stay readable"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "explicit local native budget metadata measurements; run alone with --ignored --nocapture"]
    async fn native_budget_metadata_measurements() {
        use serde_json::json;
        use std::os::fd::AsRawFd;

        // Flat-fixture read/quota logic from base 87e87412. Accounting and
        // cache implementation are shared; only the metadata reader differs.
        fn reference_read(state: &Path, total: u64) -> io::Result<(u64, u64)> {
            let file = state.join(FILE);
            assert!(file.exists());
            let budget: Budget =
                serde_json::from_slice(&std::fs::read(file)?).map_err(io::Error::other)?;
            let remaining =
                (budget.limits.disk_mi_b * 1_048_576).saturating_sub(disk_bytes(state)?);
            let policy: crate::storage::policy::Policy =
                match std::fs::read(state.join("storage-policy.json")) {
                    Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        crate::storage::policy::Policy::default()
                    }
                    Err(error) => return Err(error),
                };
            Ok((
                total,
                total.min(policy.reserve(total).saturating_add(remaining)),
            ))
        }

        fn cpu_us() -> f64 {
            let mut time = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            assert_eq!(
                unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
                0
            );
            time.tv_sec as f64 * 1_000_000.0 + time.tv_nsec as f64 / 1000.0
        }
        fn summary(mut values: Vec<f64>) -> serde_json::Value {
            values.sort_by(f64::total_cmp);
            json!({
                "p50Us": values[values.len() / 2],
                "p95Us": values[(values.len() * 95 / 100).min(values.len() - 1)],
                "meanUs": values.iter().sum::<f64>() / values.len() as f64,
            })
        }
        let parent = std::env::var_os("CAIRN_MEASUREMENT_DIR")
            .map_or_else(std::env::temp_dir, PathBuf::from);
        let root = tempfile::tempdir_in(parent).unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("native-accounting.json"),
            serde_json::to_vec(&json!({ "data": data.path() })).unwrap(),
        )
        .unwrap();
        let file = root.path().join(FILE);
        let legacy =
            json!({ "slots": 2, "limits": { "cpu": 2, "memoryMiB": 1024, "diskMiB": 512 } });
        let mut current = legacy.clone();
        current["nativeModeProtocol"] = 1.into();
        let total = 128 * 1024 * 1_048_576;
        let mut results = Vec::new();
        for (name, record) in [("legacy-reference", legacy), ("native-protocol-1", current)] {
            let encoded = serde_json::to_vec(&record).unwrap();
            crate::native::atomic_write(&file, &encoded).await.unwrap();
            let read = || {
                if name == "legacy-reference" {
                    reference_read(root.path(), total)
                } else {
                    disk_space(root.path(), total, total)
                }
            };
            for _ in 0..2 {
                read().unwrap();
            }
            let mut warm = Vec::new();
            let mut cold = Vec::new();
            let mut writes = Vec::new();
            let cpu = cpu_us();
            for _ in 0..16 {
                let begin = Instant::now();
                read().unwrap();
                warm.push(begin.elapsed().as_secs_f64() * 1_000_000.0);
                let descriptor = std::fs::File::open(&file).unwrap();
                assert_eq!(
                    unsafe {
                        libc::posix_fadvise(descriptor.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                    },
                    0
                );
                let begin = Instant::now();
                read().unwrap();
                cold.push(begin.elapsed().as_secs_f64() * 1_000_000.0);
                let begin = Instant::now();
                crate::native::atomic_write(&file, &encoded).await.unwrap();
                writes.push(begin.elapsed().as_secs_f64() * 1_000_000.0);
            }
            results.push(json!({
                "case": name,
                "encodedBytes": encoded.len(),
                "warmRead": summary(warm),
                "coldKernelEvictionRequestedRead": summary(cold),
                "durableWrite": summary(writes),
                "processCpuUs": cpu_us() - cpu,
                "trials": 16,
            }));
        }
        println!("{}", json!({ "nativeBudgetMetadata": results }));
    }

    #[test]
    fn allocation_scan_tolerates_concurrent_cleanup() {
        let root = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for i in 0..1000 {
                    let path = root.path().join(format!("disk-{i}"));
                    std::fs::create_dir(&path).unwrap();
                    std::fs::write(path.join("journal"), [1; 4096]).unwrap();
                    std::fs::remove_dir_all(path).unwrap();
                }
            });
            for _ in 0..1000 {
                allocated(root.path()).unwrap();
            }
        });
        assert_eq!(allocated(root.path()).unwrap(), 0);
    }
}
