//! Native lifecycle/occupancy fixtures. Actual Slack provider/manager recovery
//! is proved separately by slack::recovery, without seeded execution results.
#[path = "agent_effects.rs"]
mod agent_effects;

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
        Self::with_runner(origin, "http://runner:4311").await
    }

    async fn with_runner(origin: Option<String>, runner: &str) -> Self {
        let (root, s, instance, resource, app) = configured_fixture_with_runner(None, runner).await;
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
async fn workspace_effects_refuse_the_revoked_external_source_of_a_running_turn() {
    let f = Started::new(None).await;
    let admitted = json!({"status": "needs_input", "reason": "Original author", "evidence": []});
    cairn_installation::outcome::report(&f.s, &f.bearer, &admitted)
        .await
        .unwrap();
    let route = f.delivery["routing"]["id"].as_str().unwrap().to_owned();
    f.s.store
        .transaction(move |db| db.remove("plugin-routes", &route))
        .await
        .unwrap();

    let result = cairn_installation::project_workspaces::rpc(
        &f.s,
        &f.bearer,
        "tools/call",
        &json!({"name": "report_outcome", "arguments": {
            "status": "completed", "reason": "Retired author", "evidence": [],
        }}),
    )
    .await;
    let run = f.s.store.run(text(&f.run, "id")).await.unwrap();
    assert_eq!(
        run["outcome"]["reason"], admitted["reason"],
        "A still-running workspace bearer committed after its source route was revoked"
    );
    assert!(result.is_err() || result.unwrap()["isError"] == true);
}

#[tokio::test]
async fn workspace_publication_keeps_its_source_across_file_transfer_and_reapproval() {
    for change in ["route", "approval"] {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (waiting, resume) = (entered.clone(), release.clone());
        let guest = axum::Router::new().fallback(axum::routing::post(
            move |axum::Json(input): axum::Json<Value>| {
                let (waiting, resume) = (waiting.clone(), resume.clone());
                async move {
                    if input["path"] == "/private/held.md" {
                        waiting.notify_one();
                        resume.notified().await;
                    }
                    "# Synthetic private fixture\n"
                }
            },
        ));
        let (origin, guest) = common::serve_locally(guest).await;
        let f = Started::with_runner(None, &origin).await;
        common::set_checkpoint(
            &f.s.store,
            text(&f.run, "id"),
            json!({
                "runnerId": cairn_installation::config::id(),
            }),
        )
        .await;
        f.s.artifacts
            .publish(
                &f.s,
                &f.bearer,
                &json!({
                    "path": "/private/initial.md", "title": "Original file", "key": "original",
                }),
            )
            .await
            .unwrap();
        let before = cairn_installation::artifacts::list(&f.s, text(&f.run, "id"))
            .await
            .unwrap();
        assert_eq!(before.len(), 1);
        let (s, bearer) = (f.s.clone(), f.bearer.clone());
        let publication = tokio::spawn(async move {
            s.artifacts
                .publish(
                    &s,
                    &bearer,
                    &json!({
                        "path": "/private/held.md", "title": "Retired file", "key": "retired",
                    }),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        if change == "route" {
            let route = text(&f.delivery["routing"], "id").to_owned();
            f.s.store
                .transaction(move |db| db.remove("plugin-routes", &route))
                .await
                .unwrap();
        } else {
            let package = registry::active(&f.s, "router").await.unwrap();
            registry::approve(&f.s, "router", "1", &package.digest)
                .await
                .unwrap();
        }
        release.notify_one();
        let result = publication.await.unwrap();
        guest.abort();
        assert!(
            result.is_err(),
            "Publication borrowed a new {change} authority"
        );
        assert_eq!(
            cairn_installation::artifacts::list(&f.s, text(&f.run, "id"))
                .await
                .unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn upstream_mcp_keeps_the_original_source_during_connection_and_response() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for phase in ["connection", "response"] {
        let held = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (hold, seen, waiting, resume) = (
            held.clone(),
            calls.clone(),
            entered.clone(),
            release.clone(),
        );
        let upstream = axum::Router::new().fallback(axum::routing::post(
            move |axum::Json(input): axum::Json<Value>| {
                let (hold, seen, waiting, resume) = (hold.clone(), seen.clone(), waiting.clone(), resume.clone());
                async move {
                    let method = text(&input, "method");
                    let result = if method == "server/discover" {
                        if phase == "connection" && hold.load(Ordering::SeqCst) {
                            waiting.notify_one();
                            resume.notified().await;
                        }
                        json!({"supportedVersions": [cairn_installation::mcp_client::MODERN], "capabilities": {"tools": {}}})
                    } else {
                        assert_eq!(method, "tools/call");
                        seen.fetch_add(1, Ordering::SeqCst);
                        if phase == "response" && hold.load(Ordering::SeqCst) {
                            waiting.notify_one();
                            resume.notified().await;
                        }
                        json!({"content": [{"type": "text", "text": "private-upstream-result"}]})
                    };
                    axum::Json(json!({"jsonrpc": "2.0", "id": input["id"], "result": result}))
                }
            },
        ));
        let (origin, upstream) = common::serve_locally(upstream).await;
        let f = Started::new(None).await;
        let connection =
            f.s.mcps
                .save(
                    &f.s,
                    json!({
                        "name": "Source-bound upstream", "url": origin, "allowPrivateNetwork": true,
                    }),
                    None,
                )
                .await
                .unwrap();
        let configuration = f.s.mcps.run_configuration(&f.s, &f.run).await.unwrap();
        let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
        f.s.mcps
            .grant(&f.s, text(&connection, "id"), &bearer)
            .await
            .unwrap();
        let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
        let path = format!("/mcp-gateway/{}", text(&connection, "id"));
        let invoke = |app: axum::Router, bearer: String, path: String| async move {
            let response = common::send(
                &app,
                common::request("POST", &path)
                    .header("authorization", format!("Bearer {bearer}"))
                    .header(
                        "mcp-protocol-version",
                        cairn_installation::mcp_client::MODERN,
                    )
                    .header("mcp-method", "tools/call")
                    .body(Body::from(
                        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "probe", "arguments": {}}})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
            let status = response.status();
            (status, common::read_json(response).await)
        };
        let first = invoke(app.clone(), bearer.clone(), path.clone()).await;
        assert_eq!(first.0, StatusCode::OK, "{first:?}");
        assert!(first.1.to_string().contains("private-upstream-result"));
        held.store(true, Ordering::SeqCst);
        let pending = tokio::spawn(invoke(app, bearer, path));
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        let package = registry::active(&f.s, "router").await.unwrap();
        registry::approve(&f.s, "router", "1", &package.digest)
            .await
            .unwrap();
        release.notify_one();
        let response = pending.await.unwrap();
        upstream.abort();
        assert!(
            !response.1.to_string().contains("private-upstream-result"),
            "{response:?}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if phase == "connection" { 1 } else { 2 }
        );
    }
}

#[tokio::test]
async fn local_plugin_effects_queued_on_the_writer_retain_their_original_source() {
    let mut changed = Vec::new();
    for change in [
        "source",
        "target-grant",
        "source-approval",
        "target-approval",
    ] {
        for method in ["set", "max", "delete", "after", "cancel"] {
            let f = Started::new(None).await;
            let package = registry::install(&f.s, &json!({
            "manifest": {
                "id": "local-effects", "name": "Local effects", "version": "1", "apiVersion": 2,
                "capabilities": ["storage", "timers"], "agentAccess": {},
                "tools": [{"name": "local_effects_write", "description": "Exercise an instance-owned effect",
                    "inputSchema": {"type": "object", "properties": {
                        "method": {"type": "string"}, "timer": {"type": "string"}
                    }, "required": ["method"]}}],
            },
            "source": r#"return {tool = function(event)
                local input = event.input
                if input.method == 'seed' then
                    cairn.storage.set('value', 1)
                    return cairn.timers.after(3600, 'seed', {})
                elseif input.method == 'set' then cairn.storage.set('value', 2)
                elseif input.method == 'max' then cairn.storage.max('value', 2)
                elseif input.method == 'delete' then cairn.storage.delete('value')
                elseif input.method == 'after' then return cairn.timers.after(3600, 'retired', {})
                elseif input.method == 'cancel' then cairn.timers.cancel(input.timer) end
                return true
            end}"#,
        })).await.unwrap();
            registry::approve(&f.s, "local-effects", "1", text(&package, "digest"))
                .await
                .unwrap();
            let instance = connections::save(
                &f.s,
                "local-effects",
                &json!({
                    "name": "Granted local state", "enabled": true,
                    "agentIds": [f.run["snapshot"]["agent"]["id"]],
                }),
                None,
            )
            .await
            .unwrap();
            let timer = agents::call_tool(
                &f.s,
                &f.bearer,
                "local_effects_write",
                &json!({"method": "seed"}),
            )
            .await
            .unwrap()
            .unwrap();
            let before_storage = f.s.store.list("plugin-storage").await.unwrap();
            let before_timers = f.s.store.list("plugin-timers").await.unwrap();
            assert_eq!(before_storage.len(), 1);
            assert_eq!(before_timers.len(), 1);
            assert_eq!(before_storage[0]["connectionId"], instance["id"]);

            // WAL readers remain available. The real worker can finish its admission
            // reads while its local effect waits behind this transaction's writer.
            let (release, blocked) = std::sync::mpsc::channel();
            let (entered, ready) = tokio::sync::oneshot::channel();
            let s = f.s.clone();
            let route = f.delivery["routing"]["id"].as_str().unwrap().to_owned();
            let target = text(&instance, "id").to_owned();
            let writer = tokio::spawn(async move {
                s.store
                    .transaction(move |db| {
                        entered.send(()).unwrap();
                        blocked
                            .recv_timeout(std::time::Duration::from_secs(10))
                            .unwrap();
                        match change {
                            "source" => db.remove("plugin-routes", &route)?,
                            "target-grant" => {
                                let mut target = db.get(connections::KIND, &target)?.unwrap();
                                target["agentIds"] = json!([]);
                                db.put(connections::KIND, &target)?;
                            }
                            _ => {
                                let plugin = if change == "source-approval" {
                                    "router"
                                } else {
                                    "local-effects"
                                };
                                let key = format!("package:{plugin}");
                                let mut package = db.get("plugin-packages", &key)?.unwrap();
                                package["revision"] = json!(cairn_installation::config::id());
                                db.put("plugin-packages", &package)?;
                            }
                        }
                        Ok(())
                    })
                    .await
                    .unwrap();
            });
            ready.await.unwrap();
            let s = f.s.clone();
            let bearer = f.bearer.clone();
            let input = json!({"method": method, "timer": timer});
            let effect = tokio::spawn(async move {
                agents::call_tool(&s, &bearer, "local_effects_write", &input).await
            });
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let pending = !effect.is_finished();
            release.send(()).unwrap();
            writer.await.unwrap();
            let result = effect.await.unwrap();
            assert!(pending, "The {method} fixture did not wait for the writer");
            assert!(
                result.is_err(),
                "The retired invocation returned success: {method}"
            );
            if f.s.store.list("plugin-storage").await.unwrap() != before_storage
                || f.s.store.list("plugin-timers").await.unwrap() != before_timers
            {
                changed.push(format!("{change}/{method}"));
            }
        }
    }
    assert!(
        changed.is_empty(),
        "Refused invocations still changed persistent state: {changed:?}"
    );
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
