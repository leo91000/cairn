use crate::common::AgentFixture;

use super::*;

use super::management;

#[derive(Clone, Copy, Debug)]
enum Surface {
    Http,
    PublicMcp,
    Installation,
}

const SURFACES: [Surface; 3] = [Surface::Http, Surface::PublicMcp, Surface::Installation];

struct QueuedInput {
    _root: TempDir,
    service: Arc<Service>,
    app: axum::Router,
    instance: String,
    resource: String,
    route: Value,
    chat: String,
    message: String,
}

async fn queued_input() -> QueuedInput {
    let (root, service, instance, resource, app) = fixture().await;
    let agent = service
        .agent_fixture(json!({ "name": "Queued input" }), None)
        .await
        .unwrap();
    let route = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Support", "enabled": true, "match": { "channel": "C1" },
            "agentId": agent["id"], "accessMode": "requests", "userIds": ["alice"],
        }),
    )
    .await;
    let inbound = json!({
        "id": "queued-input", "resource": resource, "author": { "id": "alice" },
        "context": { "channel": "C1", "thread": "queued-input" },
        "body": { "text": "Alice's original request", "newTurn": true },
    });
    assert_eq!(
        deliver(&app, &instance, inbound).await.1["status"],
        "accepted"
    );
    let connection = connections::get(&service, "router", &instance)
        .await
        .unwrap();
    let package = registry::active(&service, "router").await.unwrap();
    let mut session = service
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();
    runtime::pump(&service, &connection, &mut session)
        .await
        .unwrap();
    session.stop().await;
    let chat = service.chat_list().await.unwrap()[0].clone();
    let detail = service.chat_detail(text(&chat, "id")).await.unwrap();
    let message = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["text"] == "Alice's original request")
        .unwrap();
    assert_eq!(message["status"], "queued");
    QueuedInput {
        _root: root,
        service,
        app,
        instance,
        resource,
        route,
        chat: text(&chat, "id").to_owned(),
        message: text(message, "id").to_owned(),
    }
}

async fn activity(input: &QueuedInput) -> Value {
    let response = common::send(
        &input.app,
        owner_request("GET", "/api/plugins", Value::Null),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let overview = common::read_json(response).await;
    overview["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == input.instance)
        .unwrap()["activity"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["deliveryId"] == input.message)
        .unwrap()
        .clone()
}

async fn change_queued_input(input: &QueuedInput, surface: Surface, withdraw: bool) {
    let message = json!({ "text": "The authenticated editor's own request" });
    if matches!(surface, Surface::Http) {
        let response = common::send(
            &input.app,
            owner_request(
                if withdraw { "DELETE" } else { "PUT" },
                &format!("/api/chats/{}/messages/{}", input.chat, input.message),
                if withdraw { Value::Null } else { message },
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_json(response).await
        );
        return;
    }
    let name = if withdraw {
        "delete_chat_message"
    } else {
        "edit_chat_message"
    };
    let mut args = json!({ "chatId": input.chat, "messageId": input.message });
    if !withdraw {
        args["message"] = message;
    }
    let response = match surface {
        Surface::PublicMcp => {
            let denied =
                management::public_call(&input.service, &["read", "manage"], name, args.clone())
                    .await;
            assert_eq!(
                denied["result"]["isError"], true,
                "Queued controls require the run scope"
            );
            management::public_call(&input.service, &["read", "run", "manage"], name, args).await
        }
        Surface::Installation => {
            let agent = management::enabled_main(&input.service).await;
            let (_, configuration) = management::active_run(&input.service, &agent).await;
            management::call(&input.service, &configuration, name, args).await
        }
        Surface::Http => unreachable!(),
    };
    management::result(&response);
}

#[tokio::test]
async fn editing_a_queued_external_input_retires_its_original_reply_destination() {
    for surface in SURFACES {
        let input = queued_input().await;
        assert_eq!(activity(&input).await["status"], "working");
        change_queued_input(&input, surface, false).await;
        let detail = input.service.chat_detail(&input.chat).await.unwrap();
        let edited = detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["id"] == input.message)
            .unwrap();
        assert_eq!(edited["text"], "The authenticated editor's own request");
        assert_eq!(edited["status"], "queued");
        assert_eq!(
            activity(&input).await["status"],
            "cancelled",
            "The previous external destination must not receive the editor's private turn"
        );
        assert!(
            input
                .service
                .store
                .get(
                    cairn_installation::plugins::work::DELIVERIES,
                    &input.message
                )
                .await
                .unwrap()
                .is_none(),
            "An edited input retained its previous external delivery"
        );
    }
}

#[tokio::test]
async fn withdrawing_an_unstarted_external_input_releases_its_delivery() {
    for surface in SURFACES {
        let input = queued_input().await;
        assert_eq!(activity(&input).await["status"], "working");
        let detail = input.service.chat_detail(&input.chat).await.unwrap();
        assert!(
            detail["run"].is_null(),
            "The scenario must withdraw before native execution"
        );
        change_queued_input(&input, surface, true).await;
        let detail = input.service.chat_detail(&input.chat).await.unwrap();
        assert!(
            !detail["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["id"] == input.message)
        );
        assert_eq!(
            activity(&input).await["status"],
            "cancelled",
            "A withdrawn request must stop reserving external delivery capacity"
        );
        assert!(
            input
                .service
                .store
                .get(
                    cairn_installation::plugins::work::DELIVERIES,
                    &input.message
                )
                .await
                .unwrap()
                .is_none(),
            "Withdrawing before launch left a delivery waiting indefinitely"
        );
    }
}

#[tokio::test]
async fn a_dormant_probe_cannot_publish_after_its_source_is_retired() {
    use cairn_installation::plugins::conditions;
    use futures_util::StreamExt;
    for source_package in [false, true] {
        for completed in [true, false] {
            let input = queued_input().await;
            let agent = input.service.get("chats", &input.chat).await.unwrap()["agentId"].clone();
            let package = registry::install(&input.service, &json!({
                "manifest": {
                    "id": "waiter", "name": "Waiter", "version": "1", "apiVersion": 2,
                    "capabilities": [], "agentAccess": {},
                    "settings": [
                        { "key": "ready", "title": "Ready", "type": "boolean" },
                        { "key": "detail", "title": "Detail", "type": "string" },
                    ],
                    "conditions": [{ "name": "signal", "description": "A controlled observation",
                        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false } }],
                },
                "source": r#"return { condition = function(event)
                    local settings = event.instances[1].settings
                    if settings.ready then
                        return {state = 'completed', marker = settings.detail, display = {title = settings.detail}}
                    end
                    return {state = 'waiting', checkAfter = 10, display = {title = settings.detail}}
                end }"#,
            })).await.unwrap();
            registry::approve(&input.service, "waiter", "1", text(&package, "digest"))
                .await
                .unwrap();
            let target = connections::save(
                &input.service,
                "waiter",
                &json!({
                    "name": "Probe", "enabled": true, "agentIds": [agent],
                    "settings": { "ready": false, "detail": "Initial observation" },
                }),
                None,
            )
            .await
            .unwrap();
            input
                .service
                .chat_tick(&std::collections::HashSet::new())
                .await
                .unwrap();
            let detail = input.service.chat_detail(&input.chat).await.unwrap();
            let run_id = text(&detail["run"], "id");
            let run = input
                .service
                .store
                .patch_run(run_id, json!({ "status": "running", "isolated": true }))
                .await
                .unwrap();
            input
                .service
                .chat_acknowledge(run_id, &input.message)
                .await
                .unwrap();
            let configuration = input
                .service
                .mcps
                .run_configuration(&input.service, &run)
                .await
                .unwrap();
            let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
            input
                .service
                .store
                .set(
                    &format!("run-checkpoint:{run_id}"),
                    json!({ "completed": true, "launched": true }),
                    None,
                )
                .await
                .unwrap();
            let scheduled = cairn_installation::chats::wait_for::schedule(
                &input.service,
                token,
                &json!({
                    "conditions": [{ "type": "waiter_signal" }], "timeoutSeconds": 90,
                    "reason": "Publish only while this source remains allowed",
                }),
            )
            .await
            .unwrap();
            assert_eq!(scheduled["deferred"], true);
            input.service.store.patch_run(run_id, json!({
                "status": "succeeded", "sessionId": "saved-session", "workspace": "/saved/workspace",
            })).await.unwrap();
            connections::save(
                &input.service,
                "waiter",
                &json!({
                    "name": "Probe", "enabled": true, "agentIds": [agent],
                    "revision": target["revision"],
                    "settings": { "ready": completed, "detail": "private-late-observation" },
                }),
                Some(text(&target, "id")),
            )
            .await
            .unwrap();
            let mut chat = input.service.get("chats", &input.chat).await.unwrap();
            chat["wakeup"]["nextCheckAt"] = 0.into();
            input.service.store.put("chats", chat).await.unwrap();
            let mut probes = Box::pin(conditions::pending(&input.service).await.unwrap());
            let observed = probes
                .next()
                .await
                .expect("The dormant condition must finish its probe");
            if source_package {
                let source = registry::active(&input.service, "router").await.unwrap();
                let mut manifest = serde_json::to_value(&source.manifest).unwrap();
                manifest["version"] = "2".into();
                registry::install(&input.service, &json!({
                    "manifest": manifest, "source": format!("-- candidate\n{}", source.source.main),
                })).await.unwrap();
                registry::approve(&input.service, "router", "1", &source.digest)
                    .await
                    .unwrap();
                assert_eq!(
                    registry::active(&input.service, "router")
                        .await
                        .unwrap()
                        .digest,
                    source.digest,
                    "The active files remain identical; the retained approval must still be exact"
                );
            } else {
                let path = format!(
                    "/api/plugins/router/connections/{}/resources/workspace/{}/routes/{}/people/alice",
                    input.instance,
                    input.resource,
                    text(&input.route, "id")
                );
                let response = common::send(
                    &input.app,
                    owner_request(
                        "POST",
                        &path,
                        json!({ "action": "remove", "revision": input.route["revision"] }),
                    ),
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
            }
            observed.publish(&input.service).await.unwrap();
            let chat = input.service.get("chats", &input.chat).await.unwrap();
            assert_eq!(
                chat["wakeup"]["observation"]["state"], "unavailable",
                "A retired source published its completed probe"
            );
            assert!(
                !chat["wakeup"]
                    .to_string()
                    .contains("private-late-observation"),
                "A retired source published private result or display metadata"
            );
            let run_id = run_id.to_owned();
            let events = input
                .service
                .store
                .read(move |db| db.events(&run_id, 0, 500))
                .await
                .unwrap();
            assert!(
                !serde_json::to_string(&events)
                    .unwrap()
                    .contains("private-late-observation")
            );
        }
    }
}
