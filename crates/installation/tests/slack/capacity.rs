//! Occupancy is synthetic; accepted requests use the actual Luau/HTTP path.
//! The after-admission case releases capacity, then runs the real adapter/process
//! to produce its answer. No completed conversation or answer is seeded.
use crate::common::AgentFixture;

use super::*;

pub(super) async fn occupy(service: &Service, instance: &Value) -> Vec<String> {
    let instance = instance.clone();
    service
        .store
        .transaction(move |db| {
            let mut ids = Vec::new();
            for _ in 0..20 {
                let id = cairn_installation::config::id();
                db.put(
                    work::DELIVERIES,
                    &json!({
                        "id": id,
                        "connectionId": instance["id"],
                        "revision": instance["revision"],
                        "authorId": "UALICE",
                        "runId": cairn_installation::config::id(),
                        "reply": {},
                        "createdAt": now(),
                        "attempts": 0,
                        // Only occupy pending slots: never ask the manager to poll a
                        // nonexistent fixture run or fabricate its completion.
                        "nextAttemptAt": now() + 86_400_000,
                    }),
                )?;
                ids.push(id);
            }
            Ok(ids)
        })
        .await
        .unwrap()
}

pub(super) async fn keep_accepted_work(
    service: &Service,
    manager: &mut tokio::process::Child,
    occupied: &[String],
) {
    for _ in 0..6 {
        // Advance only the retry clock. Admission, submission and work results
        // remain produced by the actual package and native implementation.
        let forced_at = now();
        service
            .store
            .transaction(move |db| {
                for mut event in db.list(work::EVENTS)? {
                    event["nextAttemptAt"] = forced_at.into();
                    db.put(work::EVENTS, &event)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let retained = common::eventually(
            Duration::from_secs(5),
            Duration::from_millis(10),
            async || {
                let events = service.store.list(work::EVENTS).await.unwrap();
                if events.is_empty() {
                    return Some(false);
                }
                (events[0]["nextAttemptAt"].as_i64().unwrap() > forced_at + 1000).then_some(true)
            },
        )
        .await;
        if !retained {
            stop_manager(manager).await;
            panic!(
                "An already-ACKed Slack request was discarded while native work capacity was full"
            );
        }
    }
    let occupied = occupied.to_vec();
    service
        .store
        .transaction(move |db| {
            for id in occupied {
                db.remove(work::DELIVERIES, &id)?;
            }
            for mut event in db.list(work::EVENTS)? {
                event["nextAttemptAt"] = now().into();
                db.put(work::EVENTS, &event)?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

pub(super) async fn release(service: &Service, occupied: Vec<String>) {
    service
        .store
        .transaction(move |db| {
            for id in occupied {
                db.remove(work::DELIVERIES, &id)?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

pub(super) async fn reuse_submitted_request(
    service: &Service,
    instance: &Value,
    mut admission: Value,
    slack: &Arc<Slack>,
) {
    let occupied = occupy(service, instance).await;
    admission["nextAttemptAt"] = now().into();
    service.store.put(work::EVENTS, admission).await.unwrap();
    common::eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || {
            service
                .store
                .list(work::EVENTS)
                .await
                .unwrap()
                .is_empty()
                .then_some(())
        },
    )
    .await;
    assert_eq!(
        service.store.list(work::DELIVERIES).await.unwrap().len(),
        20,
        "A submitted request created a second delivery"
    );
    assert_eq!(
        slack.posts.lock().await.len(),
        1,
        "A submitted request repeated its answer"
    );
    release(service, occupied).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accepted_slack_request_survives_capacity_pressure_until_its_real_answer() {
    exchange(DeliveryScenario::CapacityAfterAdmission).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_reply_queue_refuses_admission_without_a_permanent_duplicate_marker() {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let slack = Arc::new(Slack::default());
    let provider = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(slack);
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (instance, resource) = configure(&app, &service).await;
    let agent = service
        .agent_fixture(
            json!({
                "name": "Capacity helper",
                "access": { "projects": [], "skills": [], "mcps": [], "installation": false },
            }),
            None,
        )
        .await
        .unwrap();
    let instance_id = text(&instance, "id");
    save(
        &app,
        &format!(
            "/api/plugins/slack/connections/{instance_id}/resources/workspace/{}/routes",
            text(&resource, "id")
        ),
        json!({
            "name": "Capacity route",
            "enabled": true,
            "match": { "channel": "CTEST" },
            "agentId": agent["id"],
            "accessMode": "allowlist",
            "userIds": [ "UALICE" ],
            "maxMessagesPerHour": 10,
            "maxPerPersonPerHour": 5,
        }),
    )
    .await;
    let mut occupied = occupy(&service, &instance).await;
    let envelope = json!({
        "type": "event_callback",
        "api_app_id": "ATEST",
        "team_id": "TTEST",
        "event": {
            "type": "app_mention",
            "user": "UALICE",
            "channel": "CTEST",
            "ts": format!("{}.000001", now() / 1000),
            "text": "<@UBOT> Try when there is room",
        },
    });
    let refused = ingress(&app, instance_id, envelope.clone(), SIGNING_SECRET).await;
    assert_eq!(refused, (StatusCode::SERVICE_UNAVAILABLE, "busy".into()));
    assert!(service.store.list(work::EVENTS).await.unwrap().is_empty());
    let released = occupied.pop().unwrap();
    service
        .store
        .transaction(move |db| db.remove(work::DELIVERIES, &released))
        .await
        .unwrap();
    assert_eq!(
        ingress(&app, instance_id, envelope.clone(), SIGNING_SECRET).await,
        (StatusCode::OK, "accepted".into())
    );
    assert_eq!(
        ingress(&app, instance_id, envelope, SIGNING_SECRET).await,
        (StatusCode::OK, "duplicate".into())
    );
    assert_eq!(service.store.list(work::EVENTS).await.unwrap().len(), 1);
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
#[ignore = "local queued-event storage observation; run alone with --ignored --exact"]
async fn measure_pending_event_storage() {
    use cairn_installation::store::Store;
    let mut cases = Vec::new();
    for starts_work in [false, true] {
        let root = TempDir::new().unwrap();
        let rows: Vec<_> = (0..50)
            .map(|index| {
                let mut row = json!({
                    "id": format!("{index:08}-0000-4000-8000-000000000001"),
                    "connectionId": "00000000-0000-4000-8000-000000000002",
                    "revision": "00000000-0000-4000-8000-000000000003",
                    "name": "message",
                    "routing": null,
                    "authorId": "UALICE",
                    "payload": {
                        "id": format!("message:TTEST:CTEST:1700000000.{index:06}"),
                        "author": { "id": "UALICE", "name": "Alice" },
                        "text": "Fixture accepted request",
                        "context": { "team": "TTEST", "channel": "CTEST", "thread": "1700000000.000001" },
                    },
                    "attempts": 0,
                    "createdAt": 1700000000000_i64,
                    "nextAttemptAt": 1700000000000_i64,
                });
                if starts_work {
                    row["startsWork"] = json!(true);
                }
                row
            })
            .collect();
        let bytes: usize = rows
            .iter()
            .map(|row| serde_json::to_vec(row).unwrap().len())
            .sum();
        let store = Store::open(root.path()).unwrap();
        let seed = rows.clone();
        store
            .transaction(move |db| {
                for row in seed {
                    db.put(work::EVENTS, &row)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        drop(store);
        let store = Store::open(root.path()).unwrap();
        let id = text(rows.last().unwrap(), "id").to_owned();
        let reads = store.read(move |db| {
            let mut samples = Vec::new();
            for sample in 0..6 {
                let cpu = checkpoints::cpu_ms();
                let at = Instant::now();
                let row = db.get(work::EVENTS, &id)?.unwrap();
                samples.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64() * 1000.0,
                    "processCpuMs": checkpoints::cpu_ms()-cpu, "returnedJsonBytes": serde_json::to_vec(&row)?.len()}));
            }
            Ok(samples)
        }).await.unwrap();
        let mut writes = Vec::new();
        for sample in 0..6 {
            let mut row = rows.last().unwrap().clone();
            row["nextAttemptAt"] = json!(1700000005000_i64 + sample * 5000);
            let cpu = checkpoints::cpu_ms();
            let at = Instant::now();
            store.put(work::EVENTS, row).await.unwrap();
            writes.push(
                json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": checkpoints::cpu_ms()-cpu}),
            );
        }
        cases.push(json!({"startsWork": starts_work, "records": rows.len(), "encodedJsonBytes": bytes,
            "reads": reads, "writes": writes, "retainedRows": store.list(work::EVENTS).await.unwrap().len()}));
    }
    eprintln!(
        "PENDING_EVENT_STORAGE {}",
        json!({
            "cases": cases,
            "workload": "50 queued events per case, same payload/keys; reference omits optional startsWork, candidate adds true; six reads and six nextAttemptAt replacements",
            "cache": "fresh Store after seed; same read actor/connection executes first cold and five warm reads; OS page cache retained",
            "limits": "JSON bytes exclude SQLite pages/WAL/index overhead; reads exclude actor scheduling, writes include it; both shape workloads retain all50 rows; native lifecycle/retention and authority are verified separately; no provider/network/whole-handler performance claim",
            "host": {"parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),
                "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()},
        })
    );
}
