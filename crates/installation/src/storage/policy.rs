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
        let mut applied = self.0.lock().await;
        Self::apply_locked(state, configuration, &mut applied).await
    }

    /// Reserve reductions must reach blocked journal writers even while native
    /// close owns the disk gate. Other changes retain that gate's serialization
    /// with disk opening and its cached CPU/memory limits.
    pub async fn apply_relief(&self, state: &Path, configuration: &Configuration) -> Result<bool> {
        let mut applied = self.0.lock().await;
        let previous = match applied.as_ref() {
            Some(current) if configuration.generation <= current.generation => {
                Self::apply_locked(state, configuration.clone(), &mut applied).await?;
                return Ok(true);
            }
            Some(current) => current.policy.clone(),
            None => {
                // A restarted controller has no applied generation yet. Its
                // persisted policy can still block newly opened journal I/O.
                let saved = tokio::fs::read(state.join("storage-policy.json"))
                    .await
                    .ok();
                let Some(policy) =
                    saved.and_then(|bytes| serde_json::from_slice::<Policy>(&bytes).ok())
                else {
                    return Ok(false);
                };
                policy
            }
        };
        let mut reduced = previous.clone();
        reduced.reserve_mi_b = configuration.policy.reserve_mi_b;
        reduced.reserve_percent = configuration.policy.reserve_percent;
        if reduced != configuration.policy
            || reduced.reserve_mi_b > previous.reserve_mi_b
            || reduced.reserve_percent > previous.reserve_percent
        {
            return Ok(false);
        }
        Self::apply_locked(state, configuration.clone(), &mut applied).await?;
        Ok(true)
    }

    async fn apply_locked(
        state: &Path,
        configuration: Configuration,
        applied: &mut Option<Configuration>,
    ) -> Result<()> {
        configuration.policy.validate().map_err(Error::bad)?;
        if configuration.generation > 9_007_199_254_740_991 {
            return Err(Error::bad("Invalid storage policy generation."));
        }
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
    async fn restarted_control_relaxes_only_a_compatible_persisted_policy() {
        let root = tempfile::TempDir::new().unwrap();
        let first = Configuration {
            generation: 8,
            policy: Policy::default(),
        };
        Control::default()
            .apply(root.path(), first.clone())
            .await
            .unwrap();
        let restarted = Control::default();
        let mut relief = Configuration {
            generation: 1,
            policy: Policy {
                reserve_mi_b: 64,
                reserve_percent: 1,
                ..first.policy.clone()
            },
        };
        relief.policy.memory_cache_mi_b = 128;
        assert!(!restarted.apply_relief(root.path(), &relief).await.unwrap());
        assert_eq!(restarted.current().await, None);
        relief.policy.memory_cache_mi_b = first.policy.memory_cache_mi_b;
        assert!(restarted.apply_relief(root.path(), &relief).await.unwrap());
        assert_eq!(restarted.current().await, Some(relief));
    }

    #[tokio::test]
    async fn relief_is_monotonic_and_preserves_generation_ordering() {
        let root = tempfile::TempDir::new().unwrap();
        let control = Control::default();
        let first = Configuration {
            generation: 1,
            policy: Policy {
                reserve_mi_b: 4096,
                reserve_percent: 10,
                ..Policy::default()
            },
        };
        assert!(!control.apply_relief(root.path(), &first).await.unwrap());
        assert!(!root.path().join("storage-policy.json").exists());
        control.apply(root.path(), first.clone()).await.unwrap();
        for policy in [
            Policy {
                reserve_mi_b: 8192,
                reserve_percent: 5,
                ..first.policy.clone()
            },
            Policy {
                reserve_mi_b: 2048,
                reserve_percent: 20,
                ..first.policy.clone()
            },
            Policy {
                reserve_mi_b: 2048,
                memory_cache_mi_b: 128,
                ..first.policy.clone()
            },
        ] {
            let configuration = Configuration {
                generation: 2,
                policy,
            };
            assert!(
                !control
                    .apply_relief(root.path(), &configuration)
                    .await
                    .unwrap()
            );
            assert_eq!(control.current().await, Some(first.clone()));
        }

        let second = Configuration {
            generation: 2,
            policy: Policy {
                reserve_mi_b: 128,
                reserve_percent: 2,
                ..first.policy.clone()
            },
        };
        let latest = Configuration {
            generation: 3,
            policy: Policy {
                reserve_mi_b: 64,
                reserve_percent: 1,
                ..first.policy.clone()
            },
        };
        let (second_result, latest_result) = tokio::join!(
            control.apply_relief(root.path(), &second),
            control.apply_relief(root.path(), &latest),
        );
        assert!(second_result.unwrap());
        assert!(latest_result.unwrap());
        assert!(control.apply_relief(root.path(), &first).await.unwrap());
        assert!(control.apply_relief(root.path(), &latest).await.unwrap());
        let conflict = Configuration {
            generation: 3,
            policy: first.policy.clone(),
        };
        assert_eq!(
            control
                .apply_relief(root.path(), &conflict)
                .await
                .unwrap_err()
                .status,
            409
        );
        let increase = Configuration {
            generation: 4,
            ..second
        };
        assert!(!control.apply_relief(root.path(), &increase).await.unwrap());
        assert_eq!(control.current().await, Some(latest.clone()));
        let saved: Policy = serde_json::from_slice(
            &tokio::fs::read(root.path().join("storage-policy.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved, latest.policy);
    }

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
