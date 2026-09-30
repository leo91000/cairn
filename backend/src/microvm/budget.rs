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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub slots: usize,
    pub limits: Resources,
}

impl Budget {
    pub fn validate(&self) -> Result<()> {
        self.limits.validate()?;
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
                .saturating_add(128 * 1_048_576)
                .saturating_sub(memory);
            if needed > 0 {
                // Image imports can fill the page cache. Reclaim file pages before
                // checking a decrease; never use a smaller limit to force an OOM.
                let _ = std::fs::write(
                    cgroup.join("memory.reclaim"),
                    format!("{needed} swappiness=0"),
                );
            }
            if memory_current(cgroup)?.saturating_add(128 * 1_048_576) > memory {
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
    if !matches!(namespace.trim(), "0::/" | "0::/leo-shared")
        || !Path::new("/sys/fs/cgroup/memory.max").exists()
    {
        return Err(Error::unavailable(
            "Shared budgets require a private cgroup-v2 controller container.",
        ));
    }
    let path = PathBuf::from("/run/leo-cgroup");
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
    let shared = path.join("leo-shared");
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
    if !directory.exists() {
        return Ok(0);
    }
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
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
    let bytes = allocated(&state.join("disks"))?;
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
        let budget: Budget =
            serde_json::from_slice(&std::fs::read(file)?).map_err(io::Error::other)?;
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
        let physical_extra = policy
            .reserve(total)
            .saturating_sub(policy.reserve(total.min(limit)));
        return Ok((
            total.min(limit),
            free.saturating_sub(physical_extra).min(remaining),
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

/// Reclaim free pages and guest caches when the shared host budget gets tight.
/// Ballooning is cooperative; the cgroup remains the hard host limit.
pub async fn reclaim(state: &Path) -> Result<()> {
    let jails = state.join("jails/firecracker");
    let Ok(mut entries) = tokio::fs::read_dir(jails).await else {
        return Ok(());
    };
    while let Some(entry) = entries.next_entry().await? {
        let socket = entry.path().join("root/api.sock");
        if !socket.exists() {
            continue;
        }
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
        let available = stats["available_memory"].as_u64().unwrap_or(0) / 1_048_576;
        let extra = available.saturating_sub(256).min(256);
        if extra == 0 {
            continue;
        }
        let target = stats["actual_mib"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(extra);
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
        budget.limits.memory_mi_b = 512;
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
        for run in ["first", "second"] {
            let disk = root.path().join("disks").join(run);
            std::fs::create_dir_all(&disk).unwrap();
            std::fs::write(disk.join("journal"), vec![1; 1_048_576]).unwrap();
            std::os::unix::fs::symlink(external.path(), disk.join("external")).unwrap();
        }
        assert_eq!(disk_bytes(root.path()).unwrap(), 2 * 1_048_576);
        charge_disk(root.path(), 4 * 1_048_576).unwrap();
        let (total, free) =
            disk_space(&root.path().join("disks/first"), 1_000_000_000, 900_000_000).unwrap();
        assert_eq!(total, 128 * 1_048_576);
        assert_eq!(free, 122 * 1_048_576);
    }
}
