//! Native lifecycle/occupancy fixtures. Actual Slack provider/manager recovery
//! is proved separately by slack::recovery, without seeded execution results.
use super::*;
use cairn_installation::{chats::wait_for, plugins::work};
use tokio::sync::{Mutex, Notify};

struct Started {
    _root: TempDir,
    s: Arc<Service>,
    connection: connections::Connection,
    session: Option<cairn_plugin_host::Session>,
    delivery: Value,
    run: Value,
    bearer: String,
}

impl Started {
    async fn new(origin: Option<String>) -> Self {
        let (root, s, instance, resource, app) = fixture().await;
        s.plugins.route_http_to(origin.clone());
        let agent = s
            .agent(
                json!({
                    "name": "Dedicated helper",
                    "access": { "projects": [], "installation": false },
                }),
                None,
            )
            .await
            .unwrap();
        create_route(
            &app,
            &instance,
            &resource,
            json!({
                "name": "Original source",
                "enabled": true,
                "match": { "channel": "C1" },
                "agentId": agent["id"],
                "accessMode": "allowlist",
                "userIds": ["alice"],
            }),
        )
        .await;
        let admitted = deliver(
            &app,
            &instance,
            json!({
                "id": "continuation-source",
                "resource": resource,
                "author": { "id": "alice" },
                "context": { "channel": "C1", "thread": "original-thread" },
                "body": {
                    "text": "Continue after the provider responds",
                    "holdProgress": origin.is_some(),
                },
            }),
        )
        .await;
        assert_eq!(admitted.1["status"], "accepted");
        let package = registry::active(&s, "router").await.unwrap();
        let connection = connections::get(&s, "router", &instance).await.unwrap();
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
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        let delivery = s.store.list(work::DELIVERIES).await.unwrap().pop().unwrap();
        s.chat_tick(&std::collections::HashSet::new())
            .await
            .unwrap();
        let chat = s.chat_detail(text(&delivery, "chatId")).await.unwrap();
        let run = s
            .store
            .patch_run(text(&chat["run"], "id"), json!({ "status": "running" }))
            .await
            .unwrap();
        s.chat_acknowledge(text(&run, "id"), text(&delivery, "id"))
            .await
            .unwrap();
        let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
        let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
        Self {
            _root: root,
            s,
            connection,
            session: Some(session),
            delivery,
            run,
            bearer,
        }
    }

    async fn wait(&self, reason: &str) -> cairn_installation::error::Result<Value> {
        wait_for::schedule(
            &self.s,
            &self.bearer,
            &json!({ "conditions": [{ "type": "delay", "seconds": 3600 }], "reason": reason }),
        )
        .await
        .map(|result| {
            assert_eq!(result["deferred"], true);
            result["wakeup"].clone()
        })
    }

    async fn occupy(&self, count: usize) -> Vec<String> {
        let connection = self.connection.clone();
        self.s
            .store
            .transaction(move |db| {
                let mut ids = Vec::new();
                for _ in 0..count {
                    let id = cairn_installation::config::id();
                    db.put(
                        work::DELIVERIES,
                        &json!({
                            "id": id,
                            "connectionId": connection.id,
                            "revision": connection.revision,
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
}

#[tokio::test]
async fn a_full_queue_refuses_a_wait_atomically_and_replacement_reuses_its_slot() {
    let mut f = Started::new(None).await;
    let occupied = f.occupy(19).await;
    assert_eq!(f.wait("No slot").await.unwrap_err().status, 429);
    let chat = f.s.chat_detail(text(&f.delivery, "chatId")).await.unwrap();
    assert!(chat["wakeup"].is_null());
    assert_eq!(f.s.store.list(work::DELIVERIES).await.unwrap().len(), 20);
    let free = occupied[0].clone();
    f.s.store
        .write(move |db| db.remove(work::DELIVERIES, &free))
        .await
        .unwrap();
    let original = f.wait("Original wait").await.unwrap();
    assert_eq!(f.wait("Original wait").await.unwrap(), original);
    let replaced = f.wait("Replacement wait").await.unwrap();
    assert_ne!(replaced["id"], original["id"]);
    assert!(
        f.s.store
            .get(work::DELIVERIES, text(&original, "id"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.s.store.list(work::DELIVERIES).await.unwrap().len(), 20);
    // An arbitrary synthetic ID cannot acknowledge an input which never launched.
    f.s.chat_acknowledge(text(&f.run, "id"), text(&replaced, "id"))
        .await
        .unwrap();
    let run = text(&f.run, "id").to_owned();
    let receipts =
        f.s.store
            .read(move |db| {
                Ok(db
                    .events(&run, 0, 1000)?
                    .into_iter()
                    .filter(|event| event["type"] == "chat.wakeup.delivered")
                    .count())
            })
            .await
            .unwrap();
    assert_eq!(receipts, 0);
    f.session.take().unwrap().stop().await;
    f.s.shutdown.cancel();
}

#[tokio::test]
async fn a_continuation_never_inherits_a_predecessors_partial_or_uncertain_reply() {
    let mut f = Started::new(None).await;
    let mut parent = f.delivery.clone();
    parent["state"] = json!({ "chunk": 12 });
    f.s.store.put(work::DELIVERIES, parent).await.unwrap();
    let checkpoint_id = format!("delivery-checkpoint:{}", text(&f.delivery, "id"));
    let checkpoint = json!({
        "id": checkpoint_id, "connectionId": f.connection.id,
        "deliveryId": f.delivery["id"], "state": { "status": "sending", "nextChunk": 2 },
    });
    f.s.store
        .put(work::CHECKPOINTS, checkpoint.clone())
        .await
        .unwrap();
    let wakeup = f.wait("New output has its own receipt").await.unwrap();
    let child =
        f.s.store
            .get(work::DELIVERIES, text(&wakeup, "id"))
            .await
            .unwrap()
            .unwrap();
    assert!(child["state"].is_null());
    for field in ["reply", "routing", "userId", "thread"] {
        assert_eq!(child[field], f.delivery[field]);
    }
    assert_eq!(
        f.s.store.list(work::CHECKPOINTS).await.unwrap(),
        [checkpoint]
    );
    f.session.take().unwrap().stop().await;
    f.s.shutdown.cancel();
}

#[derive(Default)]
struct Gate {
    target: Mutex<String>,
    entered: Notify,
    release: Notify,
}

#[tokio::test]
async fn a_delayed_progress_result_cannot_resurrect_a_replaced_reservation() {
    let gate = Arc::new(Gate::default());
    let server_gate = gate.clone();
    let provider = axum::Router::new().route(
        "/progress-result",
        axum::routing::post(move |axum::Json(event): axum::Json<Value>| {
            let gate = server_gate.clone();
            async move {
                if event["deliveryId"] == *gate.target.lock().await {
                    gate.entered.notify_one();
                    gate.release.notified().await;
                }
                axum::Json(json!({}))
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    let mut f = Started::new(Some(origin)).await;
    let original = f.wait("Original wait").await.unwrap();
    f.occupy(18).await;
    *gate.target.lock().await = text(&original, "id").to_owned();
    let (s, connection) = (f.s.clone(), f.connection.clone());
    let mut session = f.session.take().unwrap();
    let pumping = tokio::spawn(async move {
        let result = runtime::pump(&s, &connection, &mut session).await;
        (session, result)
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let replaced = f
        .wait("Replacement while progress is awaiting HTTP")
        .await
        .unwrap();
    gate.release.notify_one();
    let (session, result) = pumping.await.unwrap();
    result.unwrap();
    let deliveries = f.s.store.list(work::DELIVERIES).await.unwrap();
    session.stop().await;
    f.s.shutdown.cancel();
    server.abort();
    assert_eq!(deliveries.len(), 20);
    assert!(!deliveries.iter().any(|row| row["id"] == original["id"]));
    assert!(deliveries.iter().any(|row| row["id"] == replaced["id"]));
}
