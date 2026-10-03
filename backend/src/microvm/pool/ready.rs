//! Eligibility and lifetime of an account-free, single-use prepared VM.
use super::{Budget, Plan};
use crate::{error::Result, microvm::plan::HOME, provider::Provider};
use std::{os::unix::fs::FileTypeExt, path::Path};

pub(super) const WARM_HEADROOM_MIB: u64 = 2048;

pub(super) fn capacity(value: Option<&str>) -> Result<usize> {
    match value {
        None => Ok(1),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|count| (1..=4).contains(count))
            .ok_or_else(|| {
                crate::error::Error::bad("LEO_READY_VM_POOL_SIZE must be between 1 and 4.")
            }),
    }
}

pub(super) fn memory_allowance(budget: &Budget) -> u64 {
    (budget.limits.memory_mi_b / 4).clamp(WARM_HEADROOM_MIB, 8192) * 1_048_576
}

pub(super) fn can_prepare(count: usize, bytes: Option<u64>, budget: &Budget) -> bool {
    // Preserve the original single-spare policy on small nodes. Additional
    // spares need their own measured allowance and conservative startup margin.
    count == 0
        || bytes.is_some_and(|bytes| {
            bytes.saturating_add(WARM_HEADROOM_MIB * 1_048_576) <= memory_allowance(budget)
        })
}

pub(super) struct Prepared {
    pub budget: Budget,
}

impl Prepared {
    pub fn compatible(&self, budget: &Budget, disk_mib: u64) -> bool {
        self.budget == *budget && disk_mib == super::DEFAULT_DISK_MIB
    }
}

/// Do not discard provider state, user configuration, or a previous thread just
/// to hit the fast path. The ordinary managed home has exactly this small policy.
pub(super) async fn eligible(plan: &Plan) -> Result<bool> {
    if plan.command().is_some()
        || !plan.as_value()["chat"].is_object()
        || plan.chat_provider() != Provider::Codex
        || plan.as_value()["chat"]["sessionId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    {
        return Ok(false);
    }
    let Some(home) = plan.import_to(HOME) else {
        return Ok(false);
    };
    let directory = home.source.join(".codex");
    match tokio::fs::symlink_metadata(&directory).await {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let mut entries = tokio::fs::read_dir(&directory).await?;
    let mut managed = false;
    let mut configured = false;
    while let Some(entry) = entries.next_entry().await? {
        let kind = entry.file_type().await?;
        match entry.file_name().to_str() {
            Some("leo-managed-auth") if kind.is_file() => {
                managed = policy_file(&entry.path(), b"1").await?;
            }
            Some("config.toml") if kind.is_file() => {
                configured =
                    policy_file(&entry.path(), b"cli_auth_credentials_store = \"file\"\n").await?;
            }
            Some("leo-auth.sock") if kind.is_socket() => {}
            _ => return Ok(false),
        }
    }
    Ok(managed && configured)
}

pub(super) async fn fresh_disk(state: &Path, run: &str) -> Result<bool> {
    let directory = crate::storage::environment::directory(state, run)?;
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        if !matches!(entry.file_name().to_str(), Some("lock" | "ownership.lock"))
            || !entry.file_type().await?.is_file()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn policy_file(path: &Path, expected: &[u8]) -> Result<bool> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    Ok(bytes == expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn additional_spares_need_measured_ram_and_startup_headroom() {
        let mut budget = Budget {
            slots: 8,
            limits: crate::nodes::Resources {
                cpu: 7,
                memory_mi_b: 36352,
                disk_mi_b: 65536,
            },
        };
        assert!(can_prepare(3, Some(5 * 1024 * 1_048_576), &budget));
        assert!(!can_prepare(3, Some(7 * 1024 * 1_048_576), &budget));
        assert!(!can_prepare(1, None, &budget));
        budget.limits.memory_mi_b = 4096;
        assert!(can_prepare(0, Some(0), &budget));
        assert!(!can_prepare(1, Some(1), &budget));
    }

    #[test]
    fn pool_configuration_is_bounded_and_opt_in() {
        assert_eq!(capacity(None).unwrap(), 1);
        assert_eq!(capacity(Some("4")).unwrap(), 4);
        for value in ["0", "5", "-1", "", "1.5", "four"] {
            assert!(capacity(Some(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn prepared_hardware_must_match_the_claim() {
        let budget = Budget {
            slots: 2,
            limits: crate::nodes::Resources {
                cpu: 3,
                memory_mi_b: 6144,
                disk_mi_b: 65536,
            },
        };
        let prepared = Prepared {
            budget: budget.clone(),
        };
        assert!(prepared.compatible(&budget, super::super::DEFAULT_DISK_MIB));
        let mut changed = budget.clone();
        changed.limits.cpu = 2;
        assert!(!prepared.compatible(&changed, super::super::DEFAULT_DISK_MIB));
        assert!(!prepared.compatible(&budget, 512));
    }

    #[tokio::test]
    async fn disk_checks_never_treat_restored_or_deleted_owner_records_as_fresh() {
        let root = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        assert!(fresh_disk(root.path(), &run).await.unwrap());
        let directory = root.path().join("disks").join(&run);
        tokio::fs::create_dir_all(&directory).await.unwrap();
        tokio::fs::write(directory.join("ownership.lock"), b"")
            .await
            .unwrap();
        assert!(fresh_disk(root.path(), &run).await.unwrap());
        for state in ["lazy", "data.ext4", "runtime.json", "restore.pending"] {
            tokio::fs::write(directory.join(state), b"saved state")
                .await
                .unwrap();
            assert!(!fresh_disk(root.path(), &run).await.unwrap());
            tokio::fs::remove_file(directory.join(state)).await.unwrap();
        }
    }

    #[tokio::test]
    async fn only_fresh_managed_homes_can_adopt_initialized_native_state() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        tokio::fs::create_dir_all(home.join(".codex"))
            .await
            .unwrap();
        tokio::fs::write(
            home.join(".codex/config.toml"),
            b"cli_auth_credentials_store = \"file\"\n",
        )
        .await
        .unwrap();
        tokio::fs::write(home.join(".codex/leo-managed-auth"), b"1")
            .await
            .unwrap();
        let mut document = json!({ "chat": { "provider": "codex" }, "imports": [{ "source": home, "target": HOME }] });
        assert!(eligible(&Plan::new(document.clone())).await.unwrap());
        for state in [
            "auth.json",
            "state_5.sqlite",
            "AGENTS.md",
            "skills",
            "plugins",
            "rules",
        ] {
            tokio::fs::write(home.join(".codex").join(state), b"previous or custom state")
                .await
                .unwrap();
            assert!(
                !eligible(&Plan::new(document.clone())).await.unwrap(),
                "{state}"
            );
            tokio::fs::remove_file(home.join(".codex").join(state))
                .await
                .unwrap();
        }
        document["chat"]["sessionId"] = "previous-thread".into();
        assert!(!eligible(&Plan::new(document.clone())).await.unwrap());
        document["chat"]
            .as_object_mut()
            .unwrap()
            .remove("sessionId");
        document["chat"]["provider"] = "claude".into();
        assert!(!eligible(&Plan::new(document)).await.unwrap());
    }
}
