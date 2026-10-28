use crate::common::AgentFixture;

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[tokio::test]
async fn native_submission_limits_do_not_grant_unlimited_retries_after_a_notification() {
    Box::pin(submission_limit_after_notification(false)).await;
}

#[tokio::test]
async fn a_confirmed_notification_receipt_keeps_a_safe_submission_retry_queued() {
    Box::pin(submission_limit_after_notification(true)).await;
}

async fn submission_limit_after_notification(receipted: bool) {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent_fixture(
            json!({"name": "Quota handler", "access": {"projects": []}}),
            None,
        )
        .await
        .unwrap();
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Support", "enabled": true, "match": {"channel": "C1"},
            "agentId": agent["id"], "userIds": ["alice"], "maxMessagesPerHour": 100,
        }),
    )
    .await;
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    let package = registry::active(&s, "router").await.unwrap();
    let mut session = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();
    // Fill the real delivery queue, without depending on an hourly-clock boundary.
    for index in 0..20 {
        assert_eq!(deliver(&app, &instance, json!({
            "id": format!("primer-{index}"), "resource": resource, "author": {"id": "alice"},
            "context": {"channel": "C1", "thread": format!("quota-primer-{index}")},
            "body": {"text": "Keep this pending answer"},
        })).await.1["status"], "accepted");
        runtime::pump(&s, &connection, &mut session).await.unwrap();
    }
    assert_eq!(s.store.list("plugin-deliveries").await.unwrap().len(), 20);
    assert!(s.store.list("plugin-events").await.unwrap().is_empty());
    let writes = Arc::new(AtomicUsize::new(0));
    let sent = writes.clone();
    let provider = axum::Router::new().route(
        "/quota-notification",
        axum::routing::post(move || {
            let sent = sent.clone();
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"ok": true}))
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    assert_eq!(deliver(&app, &instance, json!({
        "id": "notification-before-submit", "resource": resource, "author": {"id": "alice"},
        "context": {"channel": "C1", "thread": "quota-thread"},
        "body": {"text": "Notify then submit", "notifyThenSubmit": true, "receipted": receipted},
    })).await.1["status"], "accepted");
    for _ in 0..7 {
        let mut pending = s.store.list("plugin-events").await.unwrap();
        if pending.is_empty() {
            break;
        }
        assert_eq!(pending.len(), 1);
        pending[0]["nextAttemptAt"] = json!(0);
        s.store
            .put("plugin-events", pending.remove(0))
            .await
            .unwrap();
        runtime::pump(&s, &connection, &mut session).await.unwrap();
    }
    let pending = s.store.list("plugin-events").await.unwrap();
    if receipted {
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "A confirmed write was replayed"
        );
        assert_eq!(
            pending.len(),
            1,
            "Safe receipt replay lost the queued submission"
        );
    } else {
        assert!(
            pending.is_empty(),
            "A native 429 granted unlimited retries after a notification"
        );
        assert_eq!(
            writes.load(Ordering::SeqCst),
            5,
            "Post-effect failures must retain the ordinary finite handler budget"
        );
    }
    assert_eq!(s.store.list("plugin-deliveries").await.unwrap().len(), 20);
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn accepted_events_survive_owner_policy_outages_without_spending_handler_retries() {
    Box::pin(owner_policy_outage(1)).await;
}

#[tokio::test]
async fn accepted_events_survive_authority_outages_during_the_first_capability() {
    for failing_lookup in [2, 3, 4] {
        Box::pin(owner_policy_outage(failing_lookup)).await;
    }
}

async fn owner_policy_outage(failing_lookup: usize) {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent_fixture(
            json!({"name": "Routed outage", "access": {"projects": [], "installation": false}}),
            None,
        )
        .await
        .unwrap();
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Support", "enabled": true, "match": {"channel": "C1"},
            "agentId": agent["id"], "userIds": ["alice"],
        }),
    )
    .await;
    let inbound = json!({
        "id": "accepted-before-outage", "resource": resource,
        "author": {"id": "alice"}, "context": {"channel": "C1", "thread": "outage-thread"},
        "body": {"text": "Keep this accepted request"},
    });
    assert_eq!(
        deliver(&app, &instance, inbound.clone()).await.1["status"],
        "accepted"
    );
    let admitted = s.store.list("plugin-events").await.unwrap();
    assert_eq!(admitted.len(), 1);
    let original = admitted[0].clone();

    let unavailable = Arc::new(AtomicBool::new(true));
    let lookups = Arc::new(AtomicUsize::new(0));
    let (outage, calls) = (unavailable.clone(), lookups.clone());
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let (outage, calls) = (outage.clone(), calls.clone());
            async move {
                let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if outage.load(Ordering::SeqCst) && call % failing_lookup == 0 {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Synthetic authority outage",
                    )
                        .into_response();
                }
                axum::Json(json!({
                    "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                    "members": [],
                }))
                .into_response()
            }
        }),
    );
    let (origin, server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    assert!(connection.agent_id.is_empty());
    let package = registry::active(&s, "router").await.unwrap();
    let mut session = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();

    for cycle in 0..6 {
        let mut pending = s.store.list("plugin-events").await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "accepted content disappeared before outage cycle {cycle}"
        );
        pending[0]["nextAttemptAt"] = json!(0);
        s.store
            .put("plugin-events", pending.remove(0))
            .await
            .unwrap();
        let result = runtime::pump(&s, &connection, &mut session).await;
        assert!(result.is_ok() || result.as_ref().is_err_and(|error| error.status == 503));
    }
    let pending = s.store.list("plugin-events").await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "accepted content was discarded during the authority outage"
    );
    assert_eq!(
        pending[0]["attempts"], original["attempts"],
        "authority checks spent package-handler retries"
    );
    assert_eq!(pending[0]["payload"], original["payload"]);
    assert_eq!(pending[0]["routing"], original["routing"]);
    assert!(lookups.load(Ordering::SeqCst) >= 6);
    assert!(s.chat_list().await.unwrap().is_empty());

    unavailable.store(false, Ordering::SeqCst);
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(s.store.list("plugin-events").await.unwrap().is_empty());
    let chats = s.chat_list().await.unwrap();
    assert_eq!(
        chats.len(),
        1,
        "recovery must submit exactly the accepted request"
    );
    let detail = s.chat_detail(text(&chats[0], "id")).await.unwrap();
    assert!(
        detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["text"] == "Keep this accepted request")
    );
    assert_eq!(
        deliver(&app, &instance, inbound).await.1["status"],
        "duplicate"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(s.chat_list().await.unwrap().len(), 1);
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn package_handler_failures_still_exhaust_their_finite_retry_budget() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent_fixture(
            json!({"name": "Failing handler", "access": {"projects": []}}),
            None,
        )
        .await
        .unwrap();
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Support", "enabled": true, "match": {"channel": "C1"},
            "agentId": agent["id"], "userIds": ["alice"],
        }),
    )
    .await;
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "failing-package", "resource": resource,
                "author": {"id": "alice"}, "context": {"channel": "C1", "thread": "failing-thread"},
                "body": {"text": "Do not retry a broken handler forever", "failHandler": true},
            })
        )
        .await
        .1["status"],
        "accepted"
    );
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    let package = registry::active(&s, "router").await.unwrap();
    let mut session = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();
    for failures in 1..=5 {
        let mut pending = s.store.list("plugin-events").await.unwrap();
        assert_eq!(pending.len(), 1);
        pending[0]["nextAttemptAt"] = json!(0);
        s.store
            .put("plugin-events", pending.remove(0))
            .await
            .unwrap();
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        let pending = s.store.list("plugin-events").await.unwrap();
        if failures == 5 {
            assert!(
                pending.is_empty(),
                "a broken package handler kept retrying forever"
            );
        } else {
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0]["attempts"], failures);
        }
    }
    assert!(s.chat_list().await.unwrap().is_empty());
    session.stop().await;
}

#[tokio::test]
async fn uncertain_provider_writes_do_not_acquire_unlimited_authority_retries() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent_fixture(
            json!({"name": "Uncertain write", "access": {"projects": []}}),
            None,
        )
        .await
        .unwrap();
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Support", "enabled": true, "match": {"channel": "C1"},
            "agentId": agent["id"], "userIds": ["alice"],
        }),
    )
    .await;
    let unavailable = Arc::new(AtomicBool::new(false));
    let outage = unavailable.clone();
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let outage = outage.clone();
            async move {
                if outage.load(Ordering::SeqCst) {
                    return (StatusCode::SERVICE_UNAVAILABLE, "Synthetic authority outage").into_response();
                }
                axum::Json(json!({"owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"}, "members": []})).into_response()
            }
        }),
    );
    let (origin, authority_server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let (sent, outage) = (writes.clone(), unavailable.clone());
    let provider = axum::Router::new().route(
        "/uncertain-write",
        axum::routing::post(move || {
            let (sent, outage) = (sent.clone(), outage.clone());
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                outage.store(true, Ordering::SeqCst);
                axum::Json(json!({"ok": true}))
            }
        }),
    );
    let (origin, provider_server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    assert_eq!(deliver(&app, &instance, json!({
        "id": "uncertain-provider-write", "resource": resource, "author": {"id": "alice"},
        "context": {"channel": "C1", "thread": "uncertain-thread"},
        "body": {"text": "Do not repeat an uncertain external effect", "uncertainWrite": true},
    })).await.1["status"], "accepted");
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    let package = registry::active(&s, "router").await.unwrap();
    let mut session = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();
    for failures in 1..=5 {
        unavailable.store(false, Ordering::SeqCst);
        let mut pending = s.store.list("plugin-events").await.unwrap();
        assert_eq!(pending.len(), 1);
        pending[0]["nextAttemptAt"] = json!(0);
        s.store
            .put("plugin-events", pending.remove(0))
            .await
            .unwrap();
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        let pending = s.store.list("plugin-events").await.unwrap();
        if failures == 5 {
            assert!(
                pending.is_empty(),
                "an uncertain effect acquired unlimited automatic retries"
            );
        } else {
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0]["attempts"], failures);
        }
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "an uncertain mutation was repeated"
        );
    }
    assert!(s.chat_list().await.unwrap().is_empty());
    let receipts = s.store.list("plugin-http-receipts").await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["state"], "pending");
    session.stop().await;
    authority_server.abort();
    provider_server.abort();
}

fn process_cpu_ms() -> f64 {
    let mut clock = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut clock) },
        0
    );
    clock.tv_sec as f64 * 1000.0 + clock.tv_nsec as f64 / 1_000_000.0
}

#[tokio::test]
#[ignore = "manual queue storage observation; run alone with --ignored --exact"]
async fn measure_authority_outage_queue_storage() {
    use cairn_installation::store::Store;
    use std::time::Instant;

    let mut cases = Vec::new();
    for shape in ["handler-retry-reference", "authority-retained"] {
        let directory = TempDir::new().unwrap();
        let routing = json!({
            "id": "00000000-0000-4000-8000-000000000102",
            "revision": "route-revision",
            "resource": "workspace",
            "resources": [["workspace", "resource-revision"]],
            "digest": "synthetic-digest",
            "context": {"channel": "C1"},
            "person": {"id": "alice", "revision": "person-revision"},
        });
        let rows: Vec<_> = (0..50).map(|index| json!({
            "id": format!("00000000-0000-4000-8000-{index:012}"),
            "connectionId": "00000000-0000-4000-8000-000000000100",
            "revision": "00000000-0000-4000-8000-000000000101",
            "routing": routing,
            "name": "message",
            "authorId": "alice",
            "startsWork": true,
            "payload": {
                "id": format!("request-{index:02}"),
                "text": "x".repeat(256),
                "context": {"channel": "C1", "thread": "thread"},
            },
            "attempts": if shape == "handler-retry-reference" { 4 } else { 0 },
            "createdAt": 1700000000000_i64,
            "nextAttemptAt": if shape == "handler-retry-reference" { 1700000020000_i64 } else { 1700000000000_i64 },
        })).collect();
        let encoded_bytes: usize = rows
            .iter()
            .map(|row| serde_json::to_vec(row).unwrap().len())
            .sum();
        let store = Store::open(directory.path()).unwrap();
        let seed = rows.clone();
        store
            .transaction(move |db| {
                for row in seed {
                    db.put("plugin-events", &row)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        drop(store);
        let store = Store::open(directory.path()).unwrap();
        let key = text(rows.last().unwrap(), "id").to_owned();
        let reads = store
            .read(move |db| {
                let mut samples = Vec::new();
                for sample in 0..6 {
                    let cpu = process_cpu_ms();
                    let start = Instant::now();
                    let row = db.get("plugin-events", &key)?.unwrap();
                    samples.push(json!({
                        "sample": sample,
                        "wallMs": start.elapsed().as_secs_f64() * 1000.0,
                        "processCpuMs": process_cpu_ms() - cpu,
                        "returnedJsonBytes": serde_json::to_vec(&row)?.len(),
                    }));
                }
                Ok(samples)
            })
            .await
            .unwrap();
        let mut writes = Vec::new();
        for sample in 0..6 {
            let row = rows.last().unwrap().clone();
            let cpu = process_cpu_ms();
            let start = Instant::now();
            store.put("plugin-events", row).await.unwrap();
            writes.push(json!({
                "sample": sample,
                "wallMs": start.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": process_cpu_ms() - cpu,
            }));
        }
        cases.push(json!({
            "shape": shape,
            "records": rows.len(),
            "encodedJsonBytes": encoded_bytes,
            "reads": reads,
            "writes": writes,
        }));
    }
    eprintln!(
        "AUTHORITY_OUTAGE_QUEUE_STORAGE {}",
        json!({
            "cases": cases,
            "workload": "50 synthetic plugin event records per case; old fourth-error shape versus unchanged retained admission; six reads and six same-value writes per shape",
            "cache": "fresh Store after seed; first read connection-cold followed by five reads in the same actor/connection; OS cache retained",
            "limits": "serialized JSON bytes exclude SQLite pages/WAL/indexes; reads exclude actor scheduling, writes include it; fixed case order and uncontrolled host load; this does not benchmark the dispatch/retry selector, authority/network/provider work or end-to-end recovery",
            "host": {
                "parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "loadavg": std::fs::read_to_string("/proc/loadavg").ok(),
                "ioPressure": std::fs::read_to_string("/proc/pressure/io").ok(),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),
                "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok(),
            },
        })
    );
}

#[tokio::test]
#[ignore = "manual attachment storage observation; run alone with --ignored --exact"]
async fn measure_authority_retry_attachment_storage() {
    use cairn_installation::{config::id, store::Store};
    use std::time::Instant;

    let mut cases = Vec::new();
    for shape in ["fresh-id-reference", "reused-admission-id"] {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        let s = Service::new(common::config(root.path())).await.unwrap();
        let agent = s
            .agent_fixture(json!({"name": "Storage observation"}), None)
            .await
            .unwrap();
        let chat = s
            .chat_create(json!({"agentId": agent["id"]}))
            .await
            .unwrap();
        let chat = text(&chat, "id").to_owned();
        let repeated = id();
        let bytes = vec![b'A'; 1024];
        let mut writes = Vec::new();
        for sample in 0..6 {
            let attachment = if shape == "fresh-id-reference" {
                id()
            } else {
                repeated.clone()
            };
            let cpu = process_cpu_ms();
            let start = Instant::now();
            s.store_attachment(&chat, &attachment, "synthetic.txt", &bytes)
                .await
                .unwrap();
            writes.push(json!({
                "sample": sample,
                "wallMs": start.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": process_cpu_ms() - cpu,
            }));
        }
        let prefix = format!("chat-attachment:{chat}:");
        let rows = s.store.read(move |db| db.keys(&prefix)).await.unwrap();
        let json_bytes: usize = rows
            .iter()
            .map(|(_, value)| serde_json::to_vec(value).unwrap().len())
            .sum();
        let key_bytes: usize = rows.iter().map(|(key, _)| key.len()).sum();
        let file_bytes: u64 = rows
            .iter()
            .map(|(_, value)| {
                std::fs::metadata(
                    s.config
                        .data_dir
                        .join("chat-attachments")
                        .join(&chat)
                        .join(text(value, "id")),
                )
                .unwrap()
                .len()
            })
            .sum();
        assert_eq!(
            rows.len(),
            if shape == "fresh-id-reference" { 6 } else { 1 }
        );
        assert_eq!(file_bytes, rows.len() as u64 * 1024);
        let store = Store::open(&s.config.data_dir).unwrap();
        let key = rows.last().unwrap().0.clone();
        let reads = store
            .read(move |db| {
                let mut samples = Vec::new();
                for sample in 0..6 {
                    let cpu = process_cpu_ms();
                    let start = Instant::now();
                    let value = db.kv(&key)?.unwrap();
                    samples.push(json!({
                        "sample": sample,
                        "wallMs": start.elapsed().as_secs_f64() * 1000.0,
                        "processCpuMs": process_cpu_ms() - cpu,
                        "returnedJsonBytes": serde_json::to_vec(&value)?.len(),
                    }));
                }
                Ok(samples)
            })
            .await
            .unwrap();
        cases.push(json!({
            "shape": shape, "records": rows.len(), "encodedJsonBytes": json_bytes,
            "keyBytes": key_bytes, "logicalFileBytes": file_bytes, "writes": writes, "reads": reads,
        }));
    }
    eprintln!(
        "AUTHORITY_RETRY_ATTACHMENT_STORAGE {}",
        json!({
            "cases": cases,
            "workload": "six native Service::store_attachment calls per case with identical synthetic 1 KiB files; fresh IDs versus reuse of one ID",
            "cache": "new Store connection after writes; first metadata read connection-cold followed by five in the same actor; OS cache retained",
            "limits": "UUID generation excluded; logical files and encoded metadata/keys exclude SQLite pages, WAL, indexes and filesystem allocation; reads exclude actor scheduling and writes include it; fixed case order with uncontrolled host load; excludes plugin ID derivation, dispatch, downloads, authority calls and end-to-end recovery",
            "host": {
                "parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "loadavg": std::fs::read_to_string("/proc/loadavg").ok(),
                "ioPressure": std::fs::read_to_string("/proc/pressure/io").ok(),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),
                "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok(),
            },
        })
    );
}
