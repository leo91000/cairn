//! Accepted-work recovery uses actual provider completion, never seeded replies.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_wait_returns_the_resumed_answer_to_the_original_slack_thread() {
    exchange(Scenario::Sleep).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_work_keeps_its_reply_after_twenty_four_hours_of_recovery() {
    exchange(Scenario::Aged).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_native_wait_forwards_only_its_current_continuation() {
    exchange(Scenario::Replaced).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_condition_returns_its_completed_answer_to_slack() {
    exchange(Scenario::Condition).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_wait_keeps_its_slack_destination_through_manager_restart() {
    exchange(Scenario::Restart).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_participants_queued_request_cannot_cancel_the_authors_native_wait() {
    exchange(Scenario::OtherParticipant).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delegated_task_returns_through_the_original_slack_authors_wait() {
    exchange(Scenario::Delegated).await;
}

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    Sleep,
    Replaced,
    Condition,
    Restart,
    OtherParticipant,
    Delegated,
    Aged,
}

async fn exchange(scenario: Scenario) {
    let wait = scenario != Scenario::Aged;
    let root = TempDir::new().unwrap();
    let mut config = common::config(root.path());
    config.codex_bin = common::fixture("codex.mjs");
    std::fs::create_dir_all(config.home.join(".codex")).unwrap();
    std::fs::write(config.home.join(".codex/auth.json"), r#"{"tokens":{}}"#).unwrap();
    let controller = controller::Controller::new(config.clone());
    let (controller_origin, controller_server) = common::serve_locally(controller.router()).await;
    config.runner_url = controller_origin;
    let service = Service::new(config.clone()).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let slack = Arc::new(Slack::default());
    let provider = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(slack.clone());
    let (origin, provider_server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin.clone()));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (instance, resource) = configure(&app, &service).await;
    let agent = service
        .agent(
            json!({
                "name": "Recovering Slack helper",
                "access": {
                    "projects": [], "skills": [], "mcps": [], "installation": false,
                    "delegation": scenario == Scenario::Delegated,
                },
            }),
            None,
        )
        .await
        .unwrap();
    let route = save(
        &app,
        &format!(
            "/api/plugins/slack/connections/{}/resources/workspace/{}/routes",
            text(&instance, "id"),
            text(&resource, "id")
        ),
        json!({
            "name": "Recovering authorized thread",
            "enabled": true,
            "match": { "channel": "CTEST" },
            "agentId": agent["id"],
            "accessMode": "allowlist",
            "userIds": ["UALICE", "UBOB"],
            "maxMessagesPerHour": 10,
            "maxPerPersonPerHour": 5,
        }),
    )
    .await;
    if scenario == Scenario::Condition {
        // A granted package condition keeps the externally routed agent's
        // installation access disabled. The actual condition worker observes
        // elapsed fixture time; no completion, answer or observation is seeded.
        let package = json!({
            "manifest": {
                "id": "delay-fixture",
                "name": "Delay fixture",
                "version": "1",
                "apiVersion": 2,
                "capabilities": [],
                "agentAccess": {},
                "conditions": [{
                    "name": "ready",
                    "description": "Observe the fixture deadline",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "after": { "type": "integer" } },
                        "required": ["after"],
                        "additionalProperties": false,
                    },
                }],
            },
            "source": "return {condition = function(event) return {state = if os.time() >= event.input.after then 'completed' else 'waiting'} end}",
        });
        let installed = registry::install(&service, &package).await.unwrap();
        registry::approve(&service, "delay-fixture", "1", text(&installed, "digest"))
            .await
            .unwrap();
        connections::save(
            &service,
            "delay-fixture",
            &json!({
                "name": "Authorized timer condition",
                "enabled": true,
                "agentIds": [agent["id"]],
            }),
            None,
        )
        .await
        .unwrap();
    }
    let timestamp = format!("{}.000001", now() / 1000);
    let marker = if wait {
        "SLACK_RESUMED_FINAL"
    } else {
        "SLACK_ACCEPTED_FINAL"
    };
    let prompt = if wait {
        "fixture:stream Complete the original Slack task".to_owned()
    } else {
        format!("fixture:stream Complete {marker}")
    };
    let envelope = json!({
        "type": "event_callback",
        "api_app_id": "ATEST",
        "team_id": "TTEST",
        "event_id": "EvRECOVERY",
        "event": {
            "type": "app_mention",
            "user": "UALICE",
            "channel": "CTEST",
            "ts": timestamp,
            "text": format!("<@UBOT> {prompt}"),
        },
    });
    assert_eq!(
        ingress(
            &app,
            text(&instance, "id"),
            envelope.clone(),
            SIGNING_SECRET
        )
        .await,
        (StatusCode::OK, "accepted".to_owned())
    );
    std::fs::write(
        config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
    )
    .unwrap();
    config.worker_enabled = true;
    let file = root.path().join("config.json");
    std::fs::write(&file, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut manager = start_manager(&file, &origin).await;
    // Wait for the actual native input acknowledgement while the provider is
    // streaming. Nothing inserts a completed run, checkpoint or model answer.
    let (delivery, run) = common::eventually(
        Duration::from_secs(30),
        Duration::from_millis(20),
        async || {
            let delivery = service.store.list(work::DELIVERIES).await.ok()?.pop()?;
            let chat = service.get("chats", text(&delivery, "chatId")).await.ok()?;
            let run = service.store.run(text(&chat, "runId")).await.ok()?;
            let run_id = text(&run, "id").to_owned();
            let message = text(&delivery, "id").to_owned();
            let acknowledged = service
                .store
                .read(move |db| {
                    Ok(db.0.query_row(
                        "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND type='chat.user' AND json_extract(payload,'$.messageId')=?2)",
                        rusqlite::params![run_id, message],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await
                .ok()?;
            (acknowledged && run["status"] == "running").then_some((delivery, run))
        },
    )
    .await;
    let mut continuation_id = None;
    let mut delegated_id = None;
    if wait {
        let configuration = service
            .mcps
            .run_configuration(&service, &run)
            .await
            .unwrap();
        let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
        let reason = format!("Return {marker} to the source Slack thread");
        let wakeup = if scenario == Scenario::Delegated {
            let child = cairn_installation::delegation::delegate(
                &service,
                bearer,
                &json!({ "task": "fixture:stream Complete SLACK_DELEGATED_REPORT" }),
            )
            .await
            .unwrap();
            delegated_id = Some(text(&child, "chatId").to_owned());
            let scheduled = cairn_installation::chats::wait_for::schedule(
                &service,
                bearer,
                &json!({
                    "conditions": [{ "type": "task_finished", "chatId": child["chatId"] }],
                    "timeoutSeconds": 120, "reason": reason,
                }),
            )
            .await
            .unwrap();
            assert_eq!(scheduled["deferred"], true);
            scheduled["wakeup"].clone()
        } else if scenario == Scenario::Condition {
            let scheduled = cairn_installation::chats::wait_for::schedule(
                &service,
                bearer,
                &json!({
                    "conditions": [{ "type": "delay-fixture_ready", "after": now() / 1000 + 2 }],
                    "timeoutSeconds": 120,
                    "reason": reason,
                }),
            )
            .await
            .unwrap();
            assert_eq!(scheduled["deferred"], true);
            scheduled["wakeup"].clone()
        } else {
            let duration = if scenario == Scenario::Restart {
                120
            } else {
                1
            };
            if scenario == Scenario::Replaced {
                let old = cairn_installation::chats::wait_for::schedule(
                    &service,
                    bearer,
                    &json!({
                        "conditions": [{ "type": "delay", "seconds": 3600 }], "reason": "Superseded wait must never execute",
                    }),
                )
                .await
                .unwrap();
                let replacement = cairn_installation::chats::wait_for::schedule(
                    &service,
                    bearer,
                    &json!({
                        "conditions": [{ "type": "delay", "seconds": duration }], "reason": reason,
                    }),
                )
                .await
                .unwrap();
                assert_eq!(old["deferred"], true);
                assert_eq!(replacement["deferred"], true);
                assert_ne!(old["wakeup"]["id"], replacement["wakeup"]["id"]);
                assert!(
                    service
                        .store
                        .get(work::DELIVERIES, text(&old["wakeup"], "id"))
                        .await
                        .unwrap()
                        .is_none()
                );
            }
            cairn_installation::chats::wait_for::schedule(
                &service,
                bearer,
                &json!({
                    "conditions": [{ "type": "delay", "seconds": duration }], "reason": reason,
                }),
            )
            .await
            .map(|result| {
                assert_eq!(result["deferred"], true);
                result["wakeup"].clone()
            })
            .unwrap()
        };
        assert_eq!(wakeup["messageId"], delivery["id"]);
        continuation_id = wakeup["id"].as_str().map(str::to_owned);
        assert_eq!(
            service.store.list(work::DELIVERIES).await.unwrap().len(),
            2,
            "Replacement or identical registration leaked another reservation"
        );
        if scenario == Scenario::OtherParticipant {
            let mut bob = envelope.clone();
            bob["event_id"] = json!("EvBOBWHILEWAITING");
            bob["event"]["user"] = json!("UBOB");
            bob["event"]["thread_ts"] = json!(timestamp);
            bob["event"]["ts"] = json!(format!("{}.000002", now() / 1000));
            bob["event"]["text"] = json!("<@UBOT> Complete BOB_INDEPENDENT_REQUEST");
            assert_eq!(
                ingress(&app, text(&instance, "id"), bob, SIGNING_SECRET)
                    .await
                    .0,
                StatusCode::OK
            );
            common::eventually(
                Duration::from_secs(5),
                Duration::from_millis(20),
                async || {
                    service
                        .store
                        .list(work::DELIVERIES)
                        .await
                        .ok()?
                        .iter()
                        .any(|row| row["userId"] == "UBOB")
                        .then_some(())
                },
            )
            .await;
            let current = service
                .get("chats", text(&delivery, "chatId"))
                .await
                .unwrap();
            if current["wakeup"]["id"] != wakeup["id"] {
                stop_manager(&mut manager).await;
                panic!(
                    "Another participant's independent request cancelled the author's native wait"
                );
            }
        }
        if scenario == Scenario::Restart {
            service
                .chat_pause(text(&delivery, "chatId"), true)
                .await
                .unwrap();
            common::eventually(
                Duration::from_secs(15),
                Duration::from_millis(30),
                async || {
                    let current = service.store.run(text(&run, "id")).await.ok()?;
                    (current["status"] == "succeeded").then_some(())
                },
            )
            .await;
            stop_manager(&mut manager).await;
            let chat_id = text(&delivery, "chatId").to_owned();
            service
                .store
                .transaction(move |db| {
                    let mut chat = db.get("chats", &chat_id)?.unwrap();
                    // Advance only the persisted wait deadline; the next adapter
                    // process must still generate its answer normally after restart.
                    chat["wakeup"]["wakeAt"] = json!(now() - 1000);
                    db.put("chats", &chat)
                })
                .await
                .unwrap();
            service
                .chat_pause(text(&delivery, "chatId"), false)
                .await
                .unwrap();
            manager = start_manager(&file, &origin).await;
        }
    } else {
        let delivery = delivery.clone();
        service
            .store
            .write(move |db| {
                let mut current = db.get(work::DELIVERIES, text(&delivery, "id"))?.unwrap();
                // Simulate elapsed accepted-delivery age only. The actual
                // provider process must still finish its current work normally.
                current["createdAt"] = json!(now() - 25 * 3_600_000);
                db.put(work::DELIVERIES, &current)
            })
            .await
            .unwrap();
    }
    let expected_plans = if matches!(scenario, Scenario::OtherParticipant | Scenario::Delegated) {
        3
    } else if wait {
        2
    } else {
        1
    };
    let completed = tokio::time::timeout(Duration::from_secs(35), async {
        let mut observed = false;
        loop {
            let plans = controller.plans.lock().await.len();
            let current = service.store.run(text(&run, "id")).await.unwrap();
            if scenario == Scenario::Condition
                && plans == 1
                && current["status"] == "succeeded"
                && !observed
            {
                cairn_installation::plugins::conditions::poll_once(&service)
                    .await
                    .unwrap();
                observed = true;
            }
            if plans == expected_plans && current["status"] == "succeeded" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await;
    let returned = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let posts = slack.posts.lock().await;
            let source_reply = |marker: &str| {
                posts.iter().any(|post| {
                    post["channel"] == "CTEST"
                        && post["thread_ts"] == timestamp
                        && post["text"].contains(marker)
                })
            };
            let both_finished = source_reply(marker)
                && (scenario != Scenario::OtherParticipant
                    || source_reply("BOB_INDEPENDENT_REQUEST"));
            drop(posts);
            if both_finished {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await;
    stop_manager(&mut manager).await;
    service.shutdown.cancel();
    provider_server.abort();
    controller_server.abort();
    completed.expect("The actual provider did not complete the expected native turns");
    if returned.is_err() {
        let deliveries = service.store.list(work::DELIVERIES).await.unwrap();
        let activity = service.store.list("plugin-activity").await.unwrap();
        let posts = slack.posts.lock().await.clone();
        // Synthetic fixture data only: show source correlation and provider output,
        // never the run bearer or encrypted configuration.
        panic!(
            "Missing Slack answer: posts={posts:?}; deliveries={deliveries:?}; activity={activity:?}"
        );
    }
    if wait {
        let message_id = continuation_id.as_deref().unwrap();
        // Repeated/history acknowledgements cannot move this input boundary.
        service
            .chat_acknowledge(text(&run, "id"), message_id)
            .await
            .unwrap();
        let run_id = text(&run, "id").to_owned();
        let message_id = message_id.to_owned();
        let receipts = service.store.read(move |db| {
            Ok(db.0.query_row(
                "SELECT COUNT(*) FROM events WHERE run_id=?1 AND type='chat.wakeup.delivered' AND json_extract(payload,'$.messageId')=?2",
                rusqlite::params![run_id, message_id], |row| row.get::<_, i64>(0),
            )?)
        }).await.unwrap();
        assert_eq!(receipts, 1);
    }
    let chat_id = text(&delivery, "chatId").to_owned();
    let principals = service
        .store
        .read(move |db| db.keys(&format!("plugin-message-requester:{chat_id}:")))
        .await
        .unwrap();
    assert!(!principals.is_empty());
    assert!(principals.iter().all(|(_, principal)| {
        (principal["subject"] == "UALICE"
            || (scenario == Scenario::OtherParticipant && principal["subject"] == "UBOB"))
            && principal["connectionId"] == instance["id"]
            && principal["binding"]["id"] == route["id"]
            && principal["binding"]["resource"] == resource["id"]
    }));
    if let Some(child) = delegated_id {
        let detail = service.chat_detail(&child).await.unwrap();
        assert_eq!(detail["run"]["status"], "succeeded");
        assert!(text(&detail["run"], "summary").contains("SLACK_DELEGATED_REPORT"));
        let source = service
            .store
            .read(move |db| db.keys(&format!("plugin-message-requester:{child}:")))
            .await
            .unwrap();
        assert_eq!(source.len(), 1);
        assert_eq!(source[0].1["subject"], "UALICE");
        assert_eq!(source[0].1["binding"]["id"], route["id"]);
        assert!(
            service
                .store
                .list(work::DELIVERIES)
                .await
                .unwrap()
                .iter()
                .all(|delivery| delivery["chatId"] != detail["id"])
        );
    }
    if scenario == Scenario::OtherParticipant {
        assert_eq!(
            principals
                .iter()
                .filter(|(_, p)| p["subject"] == "UBOB")
                .count(),
            1
        );
        assert_eq!(
            principals
                .iter()
                .filter(|(_, p)| p["subject"] == "UALICE")
                .count(),
            2
        );
        assert!(
            slack
                .posts
                .lock()
                .await
                .iter()
                .any(|post| post["text"].contains("BOB_INDEPENDENT_REQUEST"))
        );
    }
}
