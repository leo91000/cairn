//! Focused native authority/retention tests. These are storage fixtures, separate
//! from the actual manager/provider exchange exercised by the parent module.
use super::*;

#[tokio::test]
async fn checkpoints_cannot_escape_the_current_reply_and_are_retired_with_the_delivery() {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let instance = connections::save(
        &service,
        "slack",
        &json!({
            "name": "Checkpoint authority", "enabled": true, "userIds": ["UALICE"],
            "settings": {"appId": "ATEST", "sharingAcknowledged": true},
            "secrets": {"signingSecret": SIGNING_SECRET},
        }),
        None,
    )
    .await
    .unwrap();
    let connection = connections::get(&service, "slack", text(&instance, "id"))
        .await
        .unwrap();
    let delivery = json!({
        "id": cairn_installation::config::id(), "connectionId": connection.id,
        "revision": connection.revision, "routing": null, "userId": "UALICE",
    });
    service
        .store
        .put(work::DELIVERIES, delivery.clone())
        .await
        .unwrap();
    let mut context = capabilities::Context {
        connection: connection.clone(),
        manifest: registry::builtin("slack").unwrap().manifest,
        event: json!({"name": "reply", "id": delivery["id"], "authorId": "UALICE"}),
        link: None,
        draft: None,
        resource_grant: None,
    };
    let checkpoint = json!({"state": {"status": "sending"}});
    capabilities::checked(
        &service,
        &context,
        "delivery.checkpoint",
        checkpoint.clone(),
    )
    .await
    .unwrap();
    let saved = service.store.list(work::CHECKPOINTS).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["state"], checkpoint["state"]);

    for (field, value) in [
        ("name", json!("message")),
        ("authorId", json!("UMALLORY")),
        ("id", json!(cairn_installation::config::id())),
    ] {
        let original = context.event[field].clone();
        context.event[field] = value;
        assert_eq!(
            capabilities::checked(
                &service,
                &context,
                "delivery.checkpoint",
                checkpoint.clone()
            )
            .await
            .unwrap_err()
            .status,
            403
        );
        context.event[field] = original;
    }
    for (field, value) in [
        ("connectionId", json!(cairn_installation::config::id())),
        ("revision", json!(cairn_installation::config::id())),
        ("routing", json!({"resource": "another-resource"})),
        ("userId", json!("UMALLORY")),
    ] {
        let mut changed = delivery.clone();
        changed[field] = value;
        service.store.put(work::DELIVERIES, changed).await.unwrap();
        assert_eq!(
            capabilities::checked(
                &service,
                &context,
                "delivery.checkpoint",
                checkpoint.clone()
            )
            .await
            .unwrap_err()
            .status,
            403
        );
        service
            .store
            .put(work::DELIVERIES, delivery.clone())
            .await
            .unwrap();
    }
    assert_eq!(
        capabilities::checked(
            &service,
            &context,
            "delivery.checkpoint",
            json!({"state": "x".repeat(65_536)})
        )
        .await
        .unwrap_err()
        .status,
        413
    );
    assert_eq!(service.store.list(work::CHECKPOINTS).await.unwrap(), saved);
    let mut revoked = instance.clone();
    revoked["enabled"] = json!(false);
    service.store.put(connections::KIND, revoked).await.unwrap();
    assert_eq!(
        capabilities::checked(
            &service,
            &context,
            "delivery.checkpoint",
            checkpoint.clone()
        )
        .await
        .unwrap_err()
        .status,
        403
    );
    service
        .store
        .put(connections::KIND, instance.clone())
        .await
        .unwrap();

    // Out-of-order workers cannot shorten a stored numeric deadline.
    let retained = capabilities::checked(
        &service,
        &context,
        "storage.max",
        json!({"key": "deadline", "value": 900}),
    )
    .await
    .unwrap();
    assert_eq!(retained, json!(900));
    assert_eq!(
        capabilities::checked(
            &service,
            &context,
            "storage.max",
            json!({"key": "deadline", "value": 60})
        )
        .await
        .unwrap(),
        json!(900)
    );
    assert_eq!(
        capabilities::checked(
            &service,
            &context,
            "storage.max",
            json!({"key": "deadline", "value": "invalid"})
        )
        .await
        .unwrap_err()
        .status,
        400
    );
    capabilities::checked(
        &service,
        &context,
        "storage.set",
        json!({"key": "text", "value": "keep me"}),
    )
    .await
    .unwrap();
    assert_eq!(
        capabilities::checked(
            &service,
            &context,
            "storage.max",
            json!({"key": "text", "value": 1})
        )
        .await
        .unwrap_err()
        .status,
        400
    );

    let removed = connections::people(&service, "slack", text(&instance, "id"), "UALICE", "remove")
        .await
        .unwrap();
    assert_eq!(removed["revision"], instance["revision"]);
    assert_eq!(
        capabilities::checked(
            &service,
            &context,
            "delivery.checkpoint",
            checkpoint.clone()
        )
        .await
        .unwrap_err()
        .status,
        403,
        "a held reply checkpoint outlived its participant's removal"
    );
    service
        .store
        .put(connections::KIND, instance.clone())
        .await
        .unwrap();

    work::finish(&service, &delivery, "replied").await.unwrap();
    assert!(
        service
            .store
            .list(work::CHECKPOINTS)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        capabilities::checked(
            &service,
            &context,
            "delivery.checkpoint",
            checkpoint.clone()
        )
        .await
        .unwrap_err()
        .status,
        403
    );
    service.store.put(work::DELIVERIES, delivery).await.unwrap();
    capabilities::checked(&service, &context, "delivery.checkpoint", checkpoint)
        .await
        .unwrap();
    connections::remove(&service, "slack", text(&instance, "id"))
        .await
        .unwrap();
    assert!(
        service
            .store
            .list(work::CHECKPOINTS)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        service
            .store
            .list(work::DELIVERIES)
            .await
            .unwrap()
            .is_empty()
    );
    service.shutdown.cancel();
}

fn cpu_ms() -> f64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut value) },
        0
    );
    value.tv_sec as f64 * 1000.0 + value.tv_nsec as f64 / 1_000_000.0
}

#[tokio::test]
#[ignore = "local receipt storage comparison; run alone with --ignored --exact"]
async fn measure_reply_checkpoint_storage() {
    use cairn_installation::store::Store;
    use std::time::Instant;
    let mut cases = Vec::new();
    for (name, kind, count) in [
        ("permanent-plugin-receipts", "plugin-storage", 1000),
        ("pending-delivery-checkpoints", work::CHECKPOINTS, 50),
    ] {
        let root = TempDir::new().unwrap();
        let connection = cairn_installation::config::id();
        let mut rows = Vec::new();
        for _ in 0..count {
            let delivery = cairn_installation::config::id();
            rows.push(if kind == work::CHECKPOINTS {
                json!({"id": format!("delivery-checkpoint:{delivery}"), "connectionId": connection,
                    "deliveryId": delivery, "state": {"status": "sent", "ts": "1700000000.000001"}})
            } else {
                let key = format!("reply:{delivery}");
                json!({"id": format!("storage:{connection}:{}", cairn_installation::auth::hex_digest(&key)),
                    "connectionId": connection, "key": key, "value": {"status": "sent", "ts": "1700000000.000001"}, "updatedAt": now()})
            });
        }
        let encoded_bytes: usize = rows
            .iter()
            .map(|row| serde_json::to_vec(row).unwrap().len())
            .sum();
        let store = Store::open(root.path()).unwrap();
        let seed = rows.clone();
        let cpu = cpu_ms();
        let at = Instant::now();
        store
            .transaction(move |db| {
                for row in seed {
                    db.put(kind, &row)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let seed_wall_ms = at.elapsed().as_secs_f64() * 1000.0;
        let seed_cpu_ms = cpu_ms() - cpu;
        drop(store);
        let store = Store::open(root.path()).unwrap();
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        let id = text(rows.last().unwrap(), "id").to_owned();
        // One read actor executes all six reads: samples 1-5 use the exact same
        // connection warmed by sample 0, not another cold reader in the pool.
        reads.extend(store.read(move |db| {
            let mut reads = Vec::new();
            for sample in 0..6 {
                let cpu = cpu_ms();
                let at = Instant::now();
                let row = db.get(kind, &id)?.unwrap();
                reads.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64() * 1000.0,
                    "processCpuMs": cpu_ms() - cpu, "returnedJsonBytes": serde_json::to_vec(&row)?.len()}));
            }
            Ok(reads)
        }).await.unwrap());
        for sample in 0..6 {
            let row = rows.last().unwrap().clone();
            let cpu = cpu_ms();
            let at = Instant::now();
            store.put(kind, row).await.unwrap();
            writes.push(
                json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": cpu_ms() - cpu}),
            );
        }
        let before_cleanup = store.list(kind).await.unwrap().len();
        let cpu = cpu_ms();
        let at = Instant::now();
        if kind == work::CHECKPOINTS {
            let settled = rows.clone();
            store
                .transaction(move |db| {
                    for row in settled {
                        db.remove(kind, text(&row, "id"))?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
        let settlement_wall_ms = at.elapsed().as_secs_f64() * 1000.0;
        let settlement_cpu_ms = cpu_ms() - cpu;
        let after_settlement = store.list(kind).await.unwrap().len();
        cases.push(
            json!({"case": name, "records": count, "encodedJsonBytes": encoded_bytes,
            "seedWallMs": seed_wall_ms, "seedCpuMs": seed_cpu_ms, "reads": reads, "writes": writes,
            "beforeSettlement": before_cleanup, "afterSettlement": after_settlement,
            "settlementWallMs": settlement_wall_ms, "settlementCpuMs": settlement_cpu_ms}),
        );
    }
    eprintln!(
        "REPLY_CHECKPOINT_STORAGE {}",
        json!({
            "cases": cases,
            "workload": "1000 historical permanent receipts versus 50 concurrently pending reply checkpoints; synthetic one-instance record-shape comparison, not a throughput A/B",
            "cache": "fresh Store after seed; one read actor/connection runs sample0 cold then5warm reads; OS page cache retained; per-query times exclude actor scheduling",
            "writes": "six same-value replacements; settlement reference keeps history and checkpoint case deletes only its selected records; no network, provider, authority or whole-delivery latency claim",
            "host": {"parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),
                "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()},
        })
    );
}

#[tokio::test]
#[ignore = "local abandoned-range KV comparison; run alone with --ignored --exact"]
async fn measure_ended_reply_range_storage() {
    use cairn_installation::store::Store;
    let mut cases = Vec::new();
    for frozen in [false, true] {
        let root = TempDir::new().unwrap();
        let mut value = json!({"runId": "00000000-0000-4000-8000-000000000001", "failed": true});
        if frozen {
            value["reply"] = json!({"delivered": 1000, "end": 1002, "failed": true});
        }
        let rows: Vec<_> = (0..1000).map(|index| {
            (format!("plugin-turn-abandoned:00000000-0000-4000-8000-000000000002:{index:08}-0000-4000-8000-000000000000"), value.clone())
        }).collect();
        let bytes: usize = rows
            .iter()
            .map(|(key, value)| key.len() + serde_json::to_vec(value).unwrap().len())
            .sum();
        let store = Store::open(root.path()).unwrap();
        let seed = rows.clone();
        store
            .transaction(move |db| {
                for (key, value) in seed {
                    db.set(&key, &value, None)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        drop(store);
        let store = Store::open(root.path()).unwrap();
        let (key, value) = rows.last().unwrap().clone();
        let lookup = key.clone();
        let reads = store.read(move |db| {
            let mut samples = Vec::new();
            for sample in 0..6 {
                let cpu = cpu_ms();
                let at = Instant::now();
                let value = db.kv(&lookup)?.unwrap();
                samples.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64()*1000.0,
                    "processCpuMs": cpu_ms()-cpu, "returnedJsonBytes": serde_json::to_vec(&value)?.len()}));
            }
            Ok(samples)
        }).await.unwrap();
        let mut writes = Vec::new();
        for sample in 0..6 {
            let cpu = cpu_ms();
            let at = Instant::now();
            store.set(&key, value.clone(), None).await.unwrap();
            writes.push(
                json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64()*1000.0,
                "processCpuMs": cpu_ms()-cpu}),
            );
        }
        cases.push(
            json!({"frozenRange": frozen, "records": rows.len(), "encodedKeyAndJsonBytes": bytes,
            "reads": reads, "writes": writes}),
        );
    }
    eprintln!(
        "ENDED_REPLY_RANGE_STORAGE {}",
        json!({
            "cases": cases,
            "workload": "1000 private KV markers of the prior shape versus the same keys with an optional already-ended range; six reads and six same-value writes per case",
            "cache": "fresh Store after seeding; one read actor executes the first connection-cold and five warm reads; OS page cache retained",
            "limits": "key and serialized JSON bytes exclude SQLite pages, WAL and index overhead; reads exclude actor scheduling, writes include it; no provider/network/whole-turn/SQL-range-selection performance claim",
            "host": {"parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),
                "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()},
        })
    );
}
