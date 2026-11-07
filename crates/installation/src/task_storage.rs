//! One installation policy bounds new disk grants; saved workspaces never shrink.
use crate::{
    config::id,
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{MAX_TASK_DISK_GIB, MIN_TASK_DISK_GIB, TaskStorageLimits};

const KEY: &str = "task-storage-limits";

fn validate(value: &TaskStorageLimits) -> Result<()> {
    if !(MIN_TASK_DISK_GIB..=MAX_TASK_DISK_GIB).contains(&value.max_disk_gi_b) {
        return Err(Error::bad("Choose a whole number of GiB from 32 to 1024."));
    }
    Ok(())
}

pub(crate) fn view(db: &Db<'_>) -> Result<TaskStorageLimits> {
    let value = db.kv_as(KEY)?.unwrap_or_default();
    validate(&value)?;
    Ok(value)
}

pub(crate) fn limit_mib(db: &Db<'_>) -> Result<u64> {
    Ok(u64::from(view(db)?.max_disk_gi_b) * 1024)
}

pub(crate) fn save(
    db: &Db<'_>,
    mut input: TaskStorageLimits,
    may_increase: bool,
) -> Result<TaskStorageLimits> {
    validate(&input)?;
    let current = view(db)?;
    if current.revision != input.revision {
        return Err(Error::conflict(
            "Task storage limits changed. Reload the saved limit before trying again.",
        ));
    }
    if !may_increase && input.max_disk_gi_b > current.max_disk_gi_b {
        return Err(Error::forbidden(
            "Task agents cannot increase their installation's disk limit. Ask the installation owner to change it.",
        ));
    }
    if current.revision != "default" && input.max_disk_gi_b == current.max_disk_gi_b {
        return Ok(current);
    }
    input.revision = id();
    let value = serde_json::to_value(&input)?;
    db.set(KEY, &value, None)?;
    db.audit("installation.task-storage-limits.updated", &value)?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[tokio::test]
    async fn installation_limit_persists_and_checks_revisions_and_agent_increases() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let expected = store
            .transaction(|db| {
                let initial = view(db)?;
                assert_eq!(initial.max_disk_gi_b, 150);
                let smaller = save(
                    db,
                    TaskStorageLimits {
                        max_disk_gi_b: 75,
                        ..initial.clone()
                    },
                    true,
                )?;
                assert_ne!(initial.revision, smaller.revision);
                assert_eq!(limit_mib(db)?, 75 * 1024);
                assert_eq!(save(db, initial, true).unwrap_err().status, 409);
                assert_eq!(
                    save(
                        db,
                        TaskStorageLimits {
                            max_disk_gi_b: 200,
                            ..smaller.clone()
                        },
                        false
                    )
                    .unwrap_err()
                    .status,
                    403
                );
                assert_eq!(view(db)?, smaller);
                let reduced = save(
                    db,
                    TaskStorageLimits {
                        max_disk_gi_b: 64,
                        ..smaller
                    },
                    false,
                )?;
                save(
                    db,
                    TaskStorageLimits {
                        max_disk_gi_b: 200,
                        ..reduced
                    },
                    true,
                )
            })
            .await
            .unwrap();
        drop(store);
        let store = Store::open(root.path()).unwrap();
        assert_eq!(store.read(|db| view(db)).await.unwrap(), expected);
    }

    #[tokio::test]
    async fn invalid_limit_cannot_replace_the_installation_policy() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                for max_disk_gi_b in [0, 31, 1025, u16::MAX] {
                    let input = TaskStorageLimits {
                        max_disk_gi_b,
                        ..view(db)?
                    };
                    assert_eq!(save(db, input, true).unwrap_err().status, 400);
                    assert_eq!(view(db)?, TaskStorageLimits::default());
                }
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn explicitly_saving_the_default_persists_the_owners_choice() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let saved = save(db, TaskStorageLimits::default(), true)?;
                assert_ne!(saved.revision, "default");
                assert_eq!(db.kv_as::<TaskStorageLimits>(KEY)?, Some(saved.clone()));
                assert_eq!(save(db, saved.clone(), true)?, saved);
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Compare existing KV primitives with the validated, revision-guarded policy.
    #[tokio::test]
    #[ignore = "local task storage policy measurement; run alone with --ignored --nocapture"]
    async fn task_storage_policy_measurement() {
        use std::time::Instant;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                save(
                    db,
                    TaskStorageLimits {
                        max_disk_gi_b: 151,
                        ..view(db)?
                    },
                    true,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let cpu_before = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let started = Instant::now();
        let reference_read_us = store
            .read(|db| {
                let start = Instant::now();
                for _ in 0..1000 {
                    std::hint::black_box(db.kv(KEY)?);
                }
                Ok(start.elapsed().as_micros() / 1000)
            })
            .await
            .unwrap();
        let policy_read_us = store
            .read(|db| {
                let start = Instant::now();
                for _ in 0..1000 {
                    std::hint::black_box(view(db)?);
                }
                Ok(start.elapsed().as_micros() / 1000)
            })
            .await
            .unwrap();
        let writes = Instant::now();
        for index in 0..100 {
            store
                .transaction(move |db| {
                    db.set(
                        KEY,
                        &serde_json::json!({ "maxDiskGiB": 150 + index % 2, "revision": id() }),
                        None,
                    )
                })
                .await
                .unwrap();
        }
        let reference_write_us = writes.elapsed().as_micros() / 100;
        let writes = Instant::now();
        for index in 0..100 {
            store
                .transaction(move |db| {
                    let mut input = view(db)?;
                    input.max_disk_gi_b = 150 + index % 2;
                    save(db, input, true)?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        let policy_write_us = writes.elapsed().as_micros() / 100;
        let encoded_bytes = store
            .read(|db| {
                Ok(db.0.query_row(
                    "SELECT length(CAST(data AS BLOB)) FROM kv WHERE key=?",
                    [KEY],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        drop(store);
        let mut reference_reopen_us = 0;
        let mut policy_reopen_us = 0;
        for _ in 0..25 {
            let start = Instant::now();
            let store = Store::open(root.path()).unwrap();
            store
                .read(|db| Ok(std::hint::black_box(db.kv(KEY)?)))
                .await
                .unwrap();
            reference_reopen_us += start.elapsed().as_micros();
            drop(store);
            let start = Instant::now();
            let store = Store::open(root.path()).unwrap();
            store
                .read(|db| Ok(std::hint::black_box(view(db)?)))
                .await
                .unwrap();
            policy_reopen_us += start.elapsed().as_micros();
        }
        println!(
            "TASK_STORAGE_MEASUREMENT {}",
            serde_json::json!({
                "referenceWarmReadUs": reference_read_us,
                "policyWarmReadUs": policy_read_us,
                "referenceWriteUs": reference_write_us,
                "policyWriteUs": policy_write_us,
                "referenceReopenUs": reference_reopen_us / 25,
                "policyReopenUs": policy_reopen_us / 25,
                "encodedPolicyBytes": encoded_bytes,
                "wallMs": started.elapsed().as_millis(),
                "cpuBefore": cpu_before,
                "cpuAfter": std::fs::read_to_string("/proc/self/stat").unwrap_or_default(),
            })
        );
    }
}
