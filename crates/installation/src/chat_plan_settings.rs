//! An explicit installation opt-in admits new plan requests; accepted turns keep their boundary.
use crate::{
    config::id,
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::ChatPlanSettings;

const KEY: &str = "chat-plan-settings";

pub(crate) fn view(db: &Db<'_>) -> Result<ChatPlanSettings> {
    Ok(db.kv_as(KEY)?.unwrap_or_default())
}

pub(crate) fn require_enabled(db: &Db<'_>) -> Result<()> {
    if !view(db)?.enabled {
        return Err(Error::forbidden(
            "Plan is disabled for this installation. Enable it in installation settings before requesting a plan turn.",
        ));
    }
    Ok(())
}

pub(crate) fn save(db: &Db<'_>, mut input: ChatPlanSettings) -> Result<ChatPlanSettings> {
    let current = view(db)?;
    if current.revision != input.revision {
        return Err(Error::conflict(
            "Chat plan settings changed. Reload the saved settings before trying again.",
        ));
    }
    if current.revision != "default" && current.enabled == input.enabled {
        return Ok(current);
    }
    input.revision = id();
    let value = serde_json::to_value(&input)?;
    db.set(KEY, &value, None)?;
    db.audit("installation.chat-plan-settings.updated", &value)?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[tokio::test]
    async fn existing_installations_default_off_and_explicit_opt_in_persists() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        // An existing installation without this key gets the same default after reopening.
        store
            .transaction(|db| db.set("existing-setting", &serde_json::json!(true), None))
            .await
            .unwrap();
        drop(store);
        let store = Store::open(root.path()).unwrap();
        let enabled = store
            .transaction(|db| {
                assert_eq!(view(db)?, ChatPlanSettings::default());
                assert_eq!(require_enabled(db).unwrap_err().status, 403);
                let enabled = save(
                    db,
                    ChatPlanSettings {
                        enabled: true,
                        ..view(db)?
                    },
                )?;
                require_enabled(db)?;
                assert_eq!(
                    save(db, ChatPlanSettings::default()).unwrap_err().status,
                    409
                );
                Ok(enabled)
            })
            .await
            .unwrap();
        drop(store);
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(move |db| {
                assert_eq!(view(db)?, enabled);
                save(
                    db,
                    ChatPlanSettings {
                        enabled: false,
                        ..enabled
                    },
                )?;
                assert_eq!(require_enabled(db).unwrap_err().status, 403);
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Local policy qualification against the existing KV primitives; no network I/O.
    #[tokio::test]
    #[ignore = "local plan policy measurement; run alone with --ignored --nocapture"]
    async fn chat_plan_policy_measurement() {
        use std::time::Instant;
        fn cpu() -> f64 {
            let mut time = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            assert_eq!(
                unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
                0
            );
            time.tv_sec as f64 + time.tv_nsec as f64 / 1e9
        }
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                save(
                    db,
                    ChatPlanSettings {
                        enabled: true,
                        ..view(db)?
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let mut results = Vec::new();
        for policy in [false, true] {
            let started_cpu = cpu();
            let warm_ns = store
                .read(move |db| {
                    let start = Instant::now();
                    for _ in 0..10000 {
                        if policy {
                            std::hint::black_box(view(db)?);
                        } else {
                            std::hint::black_box(db.kv(KEY)?);
                        }
                    }
                    Ok(start.elapsed().as_nanos() / 10000)
                })
                .await
                .unwrap();
            let start = Instant::now();
            for index in 0..1000 {
                store
                    .transaction(move |db| {
                        let input = ChatPlanSettings {
                            enabled: index % 2 == 0,
                            ..view(db)?
                        };
                        if policy {
                            save(db, input)?;
                        } else {
                            db.set(
                                KEY,
                                &serde_json::to_value(ChatPlanSettings {
                                    revision: id(),
                                    ..input
                                })?,
                                None,
                            )?;
                        }
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            let write_ns = start.elapsed().as_nanos() / 1000;
            let bytes = store
                .read(|db| Ok(serde_json::to_vec(&db.kv(KEY)?)?.len()))
                .await
                .unwrap();
            results.push(serde_json::json!({ "policy": policy, "warmReadNs": warm_ns, "writeNs": write_ns, "encodedBytes": bytes, "cpuSeconds": cpu() - started_cpu }));
        }
        drop(store);
        for policy in [false, true] {
            let started_cpu = cpu();
            let start = Instant::now();
            for _ in 0..25 {
                let store = Store::open(root.path()).unwrap();
                store
                    .read(move |db| {
                        if policy {
                            std::hint::black_box(view(db)?);
                        } else {
                            std::hint::black_box(db.kv(KEY)?);
                        }
                        Ok(())
                    })
                    .await
                    .unwrap();
                drop(store);
            }
            results.push(serde_json::json!({ "policy": policy, "reopenReadNs": start.elapsed().as_nanos() / 25, "cpuSeconds": cpu() - started_cpu }));
        }
        println!(
            "CHAT_PLAN_POLICY_MEASUREMENT {}",
            serde_json::to_string(&results).unwrap()
        );
    }
}
