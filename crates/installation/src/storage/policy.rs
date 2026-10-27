//! Node-wide disk limits. Dirty journals never participate in cache eviction.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

pub use cairn_contracts::NodeStoragePolicy as Policy;

pub fn for_node(value: &serde_json::Value) -> Result<Policy> {
    let value = if value.is_object() {
        value.clone()
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };
    let policy: Policy =
        serde_json::from_value(value).map_err(|_| Error::bad("Invalid node storage limits."))?;
    policy.validate().map_err(Error::bad)?;
    Ok(policy)
}

/// Private controller packet. Only committed policy changes advance this generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Configuration {
    pub generation: u64,
    pub policy: Policy,
}

/// Shared by the VM and native controller routes; a disconnected request keeps
/// its application task alive so an older write cannot finish after a newer one.
#[derive(Clone, Default)]
pub(crate) struct Control(Arc<tokio::sync::Mutex<Option<Configuration>>>);

impl Control {
    pub async fn current(&self) -> Option<Configuration> {
        self.0.lock().await.clone()
    }

    pub async fn apply(&self, state: &Path, configuration: Configuration) -> Result<()> {
        configuration.policy.validate().map_err(Error::bad)?;
        if configuration.generation > 9_007_199_254_740_991 {
            return Err(Error::bad("Invalid storage policy generation."));
        }
        let mut applied = self.0.lock().await;
        if let Some(current) = applied.as_ref() {
            if configuration.generation < current.generation {
                return Ok(());
            }
            if configuration.generation == current.generation {
                return if configuration.policy == current.policy {
                    Ok(())
                } else {
                    Err(Error::conflict("Storage policy generation differs."))
                };
            }
        }

        let path = state.join("storage-policy.json");
        let encoded = serde_json::to_vec(&configuration.policy)?;
        if tokio::fs::read(&path).await.ok().as_ref() != Some(&encoded) {
            crate::skills::atomic_write(&path, &encoded).await?;
        }
        // Retry runtime configuration even when a previous attempt persisted the
        // file but failed before all mounted volumes accepted the policy.
        super::runtime::configure(state, &configuration.policy)?;
        *applied = Some(configuration);
        Ok(())
    }
}

pub fn space(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    // Sample pending writes before physical/quota usage. A write that finishes
    // during the query stays conservatively charged in this result. Both cache
    // fills and new writes use this same headroom calculation under admission.
    let pending = super::cache::pending_write_bytes();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    let (total, free) = crate::microvm::budget::disk_space(
        path,
        stat.f_blocks.saturating_mul(stat.f_frsize),
        stat.f_bavail.saturating_mul(stat.f_frsize),
    )?;
    Ok((total, free.saturating_sub(pending)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_saved_disks_do_not_pause_and_dirty_disks_resume_only_when_safe() {
        let policy = Policy::default();
        let total = 1024_u64.pow(4);
        assert_eq!(policy.reserve(total), total / 100 * 5);
        assert_eq!(policy.pause_reason(total, total, None, i64::MAX), None);
        assert_eq!(policy.pause_reason(total, total, Some(1000), 300999), None);
        assert_eq!(
            policy.pause_reason(total, total, Some(1000), 301000),
            Some("backup-lag")
        );
        assert_eq!(
            policy.pause_reason(total, 0, Some(1000), 301000),
            Some("disk-space")
        );
        assert_eq!(policy.pause_reason(total, total, None, 301000), None);
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    #[tokio::test]
    async fn applied_generations_ignore_late_writes_and_recover_after_controller_restart() {
        let root = tempfile::TempDir::new().unwrap();
        let control = Control::default();
        let first = Configuration {
            generation: 1,
            policy: Policy {
                memory_cache_mi_b: 4,
                ..Policy::default()
            },
        };
        let second = Configuration {
            generation: 2,
            policy: Policy {
                memory_cache_mi_b: 8,
                ..first.policy.clone()
            },
        };
        control.apply(root.path(), second.clone()).await.unwrap();
        control.apply(root.path(), first).await.unwrap();
        assert_eq!(control.current().await, Some(second.clone()));
        let saved: Policy = serde_json::from_slice(
            &tokio::fs::read(root.path().join("storage-policy.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved, second.policy);
        let equivocation = Configuration {
            generation: second.generation,
            policy: Policy {
                memory_cache_mi_b: 16,
                ..second.policy.clone()
            },
        };
        assert!(control.apply(root.path(), equivocation).await.is_err());
        assert_eq!(control.current().await, Some(second.clone()));
        let restarted = Control::default();
        assert_eq!(restarted.current().await, None);
        restarted.apply(root.path(), second.clone()).await.unwrap();
        assert_eq!(restarted.current().await, Some(second));
    }
}
