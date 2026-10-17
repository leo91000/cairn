mod common;

use aws_lc_rs::hmac;
use axum::{body::Body, http::StatusCode, response::IntoResponse};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::{MAIN_AGENT_ID, now},
    plugins::{agents, connections, registry, resources, runtime},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;

const SECRET: &str = "route-signature-fixture";

fn owner_request(method: &str, path: &str, value: Value) -> axum::extract::Request {
    let mut request = common::request(method, path)
        .header("content-type", "application/json")
        .body(axum::Json(value).into_response().into_body())
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    request
}

async fn fixture() -> (TempDir, Arc<Service>, String, String, axum::Router) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".into();
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "router",
            "name": "Router",
            "version": "1",
            "apiVersion": 2,
            "capabilities": [
                "webhooks",
                "inbound",
                "resources",
                "oauth",
                "http",
                "conversations.submit",
                "conversations.control",
            ],
            "hosts": ["api.example.test"],
            "agentAccess": {},
            "settings": [
                { "key": "signature", "title": "Signature", "type": "secret" },
                { "key": "clientId", "title": "Client ID", "type": "string" },
            ],
            "resources": [
                {
                    "name": "workspace",
                    "title": "Workspaces",
                    "singular": "Workspace",
                    "delegation": true,
                    "settings": [
                        {
                            "key": "externalId",
                            "title": "External ID",
                            "type": "string",
                            "required": true,
                        },
                    ],
                },
            ],
            "oauth": [
                {
                    "name": "personal",
                    "title": "Personal",
                    "collection": "workspace",
                    "pkce": true,
                    "authorizationUrl": "https://api.example.test/authorize",
                    "tokenUrl": "https://api.example.test/token",
                    "clientId": "clientId",
                    "scopes": ["messages:read"],
                    "token": { "accessToken": "/access_token", "scope": "/scope" },
                    "metadata": { "externalId": "/team/id" },
                    "personal": { "subject": "/authed_user/id" },
                },
            ],
            "operations": [
                {
                    "name": "read",
                    "collection": "workspace",
                    "profile": "read",
                    "requests": [{ "method": "GET", "host": "api.example.test", "path": "/messages" }],
                },
                {
                    "name": "channel-read",
                    "collection": "workspace",
                    "profile": "read",
                    "scopes": ["channel"],
                    "requests": [
                        {
                            "method": "GET",
                            "host": "api.example.test",
                            "path": "/channel",
                            "bindings": { "channel": "channel" },
                        },
                    ],
                },
            ],
            "tools": [
                {
                    "name": "router_read",
                    "description": "Read as requester",
                    "operation": "read",
                    "inputSchema": {
                        "type": "object",
                        "required": ["resource"],
                        "properties": { "resource": { "type": "string" } },
                    },
                },
                {
                    "name": "router_scope",
                    "description": "Resolve a granted scope",
                    "operation": "channel-read",
                    "inputSchema": {
                        "type": "object",
                        "required": ["resource", "channel"],
                        "properties": { "resource": { "type": "string" }, "channel": { "type": "string" } },
                    },
                },
            ],
            "webhooks": [
                {
                    "name": "events",
                    "signature": {
                        "secret": "signature",
                        "header": "x-signature",
                        "prefix": "v0=",
                        "template": "v0:{{timestamp}}:{{body}}",
                        "timestamp": { "header": "x-timestamp", "maxAgeSeconds": 300 },
                    },
                },
            ],
        },
        "source": r#"return {
            tool = function(event)
                if event.tool == 'router_scope' then return event.input end
                return cairn.http.request({url = 'https://api.example.test/messages',
                    auth = {resource = event.input.resource, oauth = 'personal'}}).json
            end,
            webhook = function(event)
                local result = cairn.inbound.deliver(cairn.json.decode(event.body))
                return {status = 200, body = cairn.json.encode(result)}
            end,
            message = function(event)
                return cairn.conversations.submit({text = event.text, thread = event.context.thread, shared = true,
                    reply = {resource = event.resource, route = event.route, channel = event.context.channel}})
            end,
            interaction = function(event)
                if event.question then
                    return cairn.conversations.answer({conversation = event.chat, delivery = event.delivery,
                        question = event.question, answers = {choice = "Yes"}})
                end
                return cairn.conversations.cancel({conversation = event.chat, delivery = event.delivery})
            end
        }"#,
    })).await.unwrap();
    registry::approve(&s, "router", "1", text(&package, "digest"))
        .await
        .unwrap();
    let connection = connections::save(
        &s,
        "router",
        &json!({
            "name": "App",
            "enabled": true,
            "secrets": { "signature": SECRET },
            "settings": { "clientId": "client-fixture" },
        }),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        connection["agentId"], "",
        "the app itself acquired an agent delegation"
    );
    let instance = text(&connection, "id");
    let resource = resources::save(
        &s,
        "router",
        instance,
        "workspace",
        &json!({ "name": "Engineering", "settings": { "externalId": "T111" } }),
        None,
    )
    .await
    .unwrap();
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    (root, s, instance.into(), text(&resource, "id").into(), app)
}

async fn create_route(app: &axum::Router, instance: &str, resource: &str, input: Value) -> Value {
    let path =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes");
    let response = common::send(app, owner_request("POST", &path, input)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    common::read_json(response).await
}

async fn deliver(app: &axum::Router, instance: &str, value: Value) -> (StatusCode, Value) {
    let body = value.to_string();
    let time = (now() / 1000).to_string();
    let signed = format!("v0:{time}:{body}");
    let key = hmac::Key::new(hmac::HMAC_SHA256, SECRET.as_bytes());
    let signature = hex::encode(hmac::sign(&key, signed.as_bytes()).as_ref());
    let request = common::request("POST", &format!("/api/plugin-webhooks/{instance}/events"))
        .header("content-type", "application/json")
        .header("x-signature", format!("v0={signature}"))
        .header("x-timestamp", time)
        .body(Body::from(body))
        .unwrap();
    let response = common::send(app, request).await;
    let status = response.status();
    let body = common::read_text(response).await;
    (
        status,
        serde_json::from_str(&body).unwrap_or(json!({ "message": body })),
    )
}

#[tokio::test]
async fn resource_routes_choose_the_owner_agent_and_keep_channels_and_workspaces_separate() {
    let (_root, s, instance, resource, app) = fixture().await;
    let first = s.agent(json!({ "name": "First", "access": { "projects": [], "installation": false, "github": false } }), None).await.unwrap();
    let second = s.agent(json!({ "name": "Second", "access": { "projects": [], "installation": false, "github": false } }), None).await.unwrap();
    for (channel, agent) in [("C1", &first), ("C2", &second)] {
        create_route(
            &app,
            &instance,
            &resource,
            json!({
                "name": channel,
                "enabled": true,
                "match": { "channel": channel },
                "agentId": agent["id"],
                "userIds": ["alice", "bob"],
                "maxMessagesPerHour": 10,
                "maxPerPersonPerHour": 5,
            }),
        )
        .await;
    }
    let other = resources::save(
        &s,
        "router",
        &instance,
        "workspace",
        &json!({ "name": "Other workspace", "settings": { "externalId": "T222" } }),
        None,
    )
    .await
    .unwrap();
    let other = text(&other, "id");
    create_route(
        &app,
        &instance,
        other,
        json!({
            "name": "Other route",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": second["id"],
            "userIds": ["alice"],
        }),
    )
    .await;
    for (id, resource, channel, author, expected) in [
        ("first", resource.as_str(), "C1", "alice", "accepted"),
        ("second", resource.as_str(), "C2", "alice", "accepted"),
        ("first", other, "C1", "alice", "accepted"),
        ("first", resource.as_str(), "C1", "alice", "duplicate"),
        ("stranger", resource.as_str(), "C1", "mallory", "denied"),
        ("outside", resource.as_str(), "C3", "alice", "denied"),
    ] {
        let (status, body) = deliver(
            &app,
            &instance,
            json!({
                "id": id,
                "resource": resource,
                "author": { "id": author },
                "context": { "channel": channel, "thread": "same-thread" },
                "body": { "text": channel, "agentId": MAIN_AGENT_ID, "route": "forged" },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], expected);
    }
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
    runtime::begin(&s, &connection, &mut session).await.unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let chats = s.chat_list().await.unwrap();
    assert_eq!(
        chats.len(),
        3,
        "channel/workspace boundaries or per-workspace deduplication were lost"
    );
    assert!(chats.iter().any(|chat| chat["agentId"] == first["id"]));
    assert!(chats.iter().any(|chat| chat["agentId"] == second["id"]));
    session.stop().await;
}

#[tokio::test]
async fn disabled_ambiguous_and_revised_routes_cannot_fall_back_or_keep_queued_authority() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": { "projects": [], "installation": false, "github": false },
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
            "name": "Default",
            "enabled": true,
            "agentId": agent["id"],
            "userIds": ["alice"],
        }),
    )
    .await;
    let blocked = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Blocked",
            "enabled": false,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "userIds": ["alice"],
        }),
    )
    .await;
    let event = |id: &str| {
        json!({
            "id": id,
            "resource": resource,
            "author": { "id": "alice" },
            "context": { "channel": "C1", "kind": "message", "thread": "thread" },
            "body": { "text": "Hello" },
        })
    };
    assert_eq!(
        deliver(&app, &instance, event("disabled")).await.1["status"],
        "denied"
    );
    let route_path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}",
        text(&blocked, "id")
    );
    assert_eq!(
        common::send(&app, owner_request("DELETE", &route_path, Value::Null))
            .await
            .status(),
        StatusCode::OK
    );
    let channel = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Channel",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "userIds": ["alice"],
        }),
    )
    .await;
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Message",
            "enabled": true,
            "match": { "kind": "message" },
            "agentId": agent["id"],
            "userIds": ["alice"],
        }),
    )
    .await;
    assert_eq!(
        deliver(&app, &instance, event("ambiguous")).await.0,
        StatusCode::CONFLICT
    );
    let input = json!({
        "name": "Specific",
        "enabled": true,
        "match": { "channel": "C1", "kind": "message" },
        "agentId": agent["id"],
        "userIds": ["alice"],
        "revision": channel["revision"],
    });
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}",
        text(&channel, "id")
    );
    let saved = common::send(&app, owner_request("PUT", &path, input)).await;
    assert_eq!(saved.status(), StatusCode::OK);
    let saved = common::read_json(saved).await;
    assert_eq!(
        deliver(&app, &instance, event("accepted-before-change"))
            .await
            .1["status"],
        "accepted"
    );
    let paused = common::send(
        &app,
        owner_request(
            "PUT",
            &path,
            json!({
                "name": "Paused",
                "enabled": false,
                "match": { "channel": "C1", "kind": "message" },
                "agentId": agent["id"],
                "userIds": ["alice"],
                "revision": saved["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(paused.status(), StatusCode::OK);
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
    assert!(
        s.chat_list().await.unwrap().is_empty(),
        "queued work retained a revoked route"
    );
    session.stop().await;
}

#[tokio::test]
async fn access_requests_and_approvals_are_isolated_to_the_selected_route() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": { "projects": [], "installation": false, "github": false },
            }),
            None,
        )
        .await
        .unwrap();
    let mut route_ids = Vec::new();
    for channel in ["C1", "C2"] {
        let route = create_route(
            &app,
            &instance,
            &resource,
            json!({
                "name": channel,
                "enabled": true,
                "match": { "channel": channel },
                "agentId": agent["id"],
                "accessMode": "requests",
                "userIds": [],
            }),
        )
        .await;
        route_ids.push(text(&route, "id").to_owned());
        assert_eq!(
            deliver(
                &app,
                &instance,
                json!({
                    "id": channel,
                    "resource": resource,
                    "author": { "id": "alice", "name": "Alice" },
                    "context": { "channel": channel },
                    "body": { "text": "private-unadmitted-content" },
                })
            )
            .await
            .1["status"],
            "requested"
        );
    }
    connections::save(
        &s,
        "router",
        &json!({"name": "Renamed app", "enabled": true, "settings": {"clientId": "client-fixture"}}),
        Some(&instance),
    ).await.unwrap();
    for route in &route_ids {
        let path = format!(
            "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{route}/requests"
        );
        let response = common::send(&app, owner_request("GET", &path, Value::Null)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = common::read_json(response).await;
        assert_eq!(response["items"].as_array().unwrap().len(), 1);
        assert_eq!(response["items"][0]["userId"], "alice");
        assert!(!response.to_string().contains("private-unadmitted-content"));
    }
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/alice",
        route_ids[0]
    );
    let approved = common::send(
        &app,
        owner_request("POST", &path, json!({ "action": "approve" })),
    )
    .await;
    assert_eq!(approved.status(), StatusCode::OK);
    for (channel, expected) in [("C1", "accepted"), ("C2", "requested")] {
        assert_eq!(
            deliver(
                &app,
                &instance,
                json!({
                    "id": format!("after-{channel}"),
                    "resource": resource,
                    "author": { "id": "alice" },
                    "context": { "channel": channel, "thread": "thread" },
                    "body": { "text": "Hello" },
                })
            )
            .await
            .1["status"],
            expected
        );
    }
}

#[tokio::test]
async fn only_the_current_delivery_author_can_stop_a_shared_resource_conversation() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": { "projects": [], "installation": false, "github": false },
            }),
            None,
        )
        .await
        .unwrap();
    for channel in ["C1", "C2"] {
        create_route(
            &app,
            &instance,
            &resource,
            json!({
                "name": channel,
                "enabled": true,
                "match": { "channel": channel },
                "agentId": agent["id"],
                "userIds": ["alice", "bob"],
            }),
        )
        .await;
    }
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "first",
                "resource": resource,
                "author": { "id": "alice" },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "text": "Hello" },
            })
        )
        .await
        .1["status"],
        "accepted"
    );
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
    let chat = s.chat_list().await.unwrap()[0].clone();
    let chat_id = text(&chat, "id");
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = s.chat_detail(chat_id).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({ "status": "running", "isolated": true }),
        )
        .await
        .unwrap();
    let message = text(&run["chatExecution"], "messageId");
    for (id, author, channel, delivery) in [
        ("other-person", "bob", "C1", message),
        ("other-route", "alice", "C2", message),
        ("old-control", "alice", "C1", "outdated-delivery"),
    ] {
        assert_eq!(
            deliver(
                &app,
                &instance,
                json!({
                    "id": id,
                    "kind": "interaction",
                    "resource": resource,
                    "author": { "id": author },
                    "context": { "channel": channel, "thread": "thread" },
                    "body": { "chat": chat_id, "delivery": delivery },
                })
            )
            .await
            .1["status"],
            "accepted"
        );
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        assert_eq!(
            s.chat_detail(chat_id).await.unwrap()["paused"],
            false,
            "an unrelated or stale action stopped the active request: {id}"
        );
    }
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "own-control",
                "kind": "interaction",
                "resource": resource,
                "author": { "id": "alice" },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "chat": chat_id, "delivery": message },
            })
        )
        .await
        .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(s.chat_detail(chat_id).await.unwrap()["paused"], true);
    session.stop().await;
}

#[tokio::test]
async fn external_question_answers_keep_the_issuing_turn_and_its_requester() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": { "projects": [], "installation": false, "github": false },
            }),
            None,
        )
        .await
        .unwrap();
    for channel in ["C1", "C2"] {
        create_route(
            &app,
            &instance,
            &resource,
            json!({
                "name": channel,
                "enabled": true,
                "match": { "channel": channel },
                "agentId": agent["id"],
                "userIds": ["alice", "bob"],
            }),
        )
        .await;
    }
    deliver(
        &app,
        &instance,
        json!({
            "id": "first",
            "resource": resource,
            "author": { "id": "alice" },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "text": "Hello" },
        }),
    )
    .await;
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
    let chat = s.chat_list().await.unwrap()[0].clone();
    let chat_id = text(&chat, "id");
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = s.chat_detail(chat_id).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({ "status": "running", "isolated": true }),
        )
        .await
        .unwrap();
    let message = text(&run["chatExecution"], "messageId");
    let question = "a".repeat(64);
    s.question_receive(
        text(&run, "id"),
        json!({
            "id": question,
            "blocking": false,
            "fields": [{ "id": "choice", "title": "Continue?", "options": [] }],
        }),
    )
    .await
    .unwrap();
    for (id, author, channel, delivery) in [
        ("other-person", "bob", "C1", message),
        ("other-route", "alice", "C2", message),
        ("old-control", "alice", "C1", "outdated-delivery"),
    ] {
        deliver(
            &app,
            &instance,
            json!({
                "id": id,
                "kind": "interaction",
                "resource": resource,
                "author": { "id": author },
                "context": { "channel": channel, "thread": "thread" },
                "body": { "chat": chat_id, "delivery": delivery, "question": question },
            }),
        )
        .await;
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        assert_eq!(
            s.chat_detail(chat_id).await.unwrap()["questions"][0]["status"],
            "pending"
        );
    }
    deliver(
        &app,
        &instance,
        json!({
            "id": "own-answer",
            "kind": "interaction",
            "resource": resource,
            "author": { "id": "alice" },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "chat": chat_id, "delivery": message, "question": question },
        }),
    )
    .await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let detail = s.chat_detail(chat_id).await.unwrap();
    assert_eq!(
        detail["questions"][0]["status"], "answering",
        "the authenticated external author could not answer their own question"
    );
    assert_eq!(detail["questions"][0]["turnId"], message);
    session.stop().await;
}

#[tokio::test]
async fn routed_runs_use_only_the_oauth_subject_proved_by_their_external_requester() {
    let (_root, s, instance, resource, app) = fixture().await;
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (held, read_entered, read_release) = (hold.clone(), entered.clone(), release.clone());
    let provider = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "access_token": "linked-owner-fixture",
                    "scope": "messages:read",
                    "team": { "id": "T111" },
                    "authed_user": { "id": "U111" },
                }))
            }),
        )
        .route(
            "/messages",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let (held, entered, release) =
                    (held.clone(), read_entered.clone(), read_release.clone());
                async move {
                    assert_eq!(headers["authorization"], "Bearer linked-owner-fixture");
                    if held.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({ "subject": "U111" }))
                }
            }),
        );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let oauth_path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/oauth/personal"
    );
    let started = common::send(
        &app,
        owner_request("POST", &oauth_path, json!({ "scopes": ["messages:read"] })),
    )
    .await;
    assert_eq!(started.status(), StatusCode::OK);
    let started = common::read_json(started).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&started, "state")
    );
    let response = common::send(
        &app,
        common::request("GET", &callback)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let finished = common::send(
        &app,
        owner_request(
            "POST",
            &format!("{oauth_path}/finish"),
            json!({ "state": started["state"] }),
        ),
    )
    .await;
    assert_eq!(finished.status(), StatusCode::OK);
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": { "projects": [], "installation": false, "github": false },
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
            "name": "Channel",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "userIds": ["U111", "U222"],
        }),
    )
    .await;
    let grants =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/grants");
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "POST",
                &grants,
                json!({ "agentId": agent["id"], "profile": "read" })
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
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
    for (author, thread, expected) in [("U111", "linked", Some("U111")), ("U222", "unlinked", None)]
    {
        assert_eq!(
            deliver(
                &app,
                &instance,
                json!({
                    "id": author,
                    "resource": resource,
                    "author": { "id": author },
                    "context": { "channel": "C1", "thread": thread },
                    "body": { "text": "Read as me" },
                })
            )
            .await
            .1["status"],
            "accepted"
        );
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        s.chat_tick(&std::collections::HashSet::new())
            .await
            .unwrap();
        let chats = s.chat_list().await.unwrap();
        let chat = chats
            .iter()
            .find(|chat| chat["status"] == "queued")
            .unwrap();
        let detail = s.chat_detail(text(chat, "id")).await.unwrap();
        let run = s
            .store
            .patch_run(
                text(&detail["run"], "id"),
                json!({ "status": "running", "isolated": true }),
            )
            .await
            .unwrap();
        let config = s.mcps.run_configuration(&s, &run).await.unwrap();
        let result = agents::call_tool(
            &s,
            text(&config["env"], "CAIRN_MCP_RUN_TOKEN"),
            "router_read",
            &json!({ "resource": resource }),
        )
        .await;
        match expected {
            Some(subject) => assert_eq!(result.unwrap().unwrap()["subject"], subject),
            None => assert!(
                result.is_err(),
                "an unlinked external person inherited the installation owner's personal token"
            ),
        }
        if expected.is_some() {
            hold.store(true, std::sync::atomic::Ordering::SeqCst);
            let (pending_service, input) = (s.clone(), json!({"resource":resource}));
            let bearer = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
            let pending = tokio::spawn(async move {
                agents::call_tool(&pending_service, &bearer, "router_read", &input).await
            });
            tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
                .await
                .unwrap();
            let disconnected =
                common::send(&app, owner_request("DELETE", &oauth_path, Value::Null)).await;
            assert_eq!(disconnected.status(), StatusCode::OK);
            release.notify_one();
            assert!(
                pending.await.unwrap().is_err(),
                "a result escaped after its personal identity was disconnected"
            );
        }
    }
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn an_external_run_cannot_select_another_workspace_or_escape_its_channel_route() {
    let (_root, s, instance, resource, app) = fixture().await;
    let other = resources::save(
        &s,
        "router",
        &instance,
        "workspace",
        &json!({ "name": "Other", "settings": { "externalId": "T222" } }),
        None,
    )
    .await
    .unwrap();
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": { "projects": [], "installation": false, "github": false },
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
            "name": "Channel",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "userIds": ["alice"],
        }),
    )
    .await;
    for resource in [resource.as_str(), text(&other, "id")] {
        let grants = format!(
            "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/grants"
        );
        assert_eq!(
            common::send(
                &app,
                owner_request(
                    "POST",
                    &grants,
                    json!({ "agentId": agent["id"], "profile": "read" })
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
    deliver(
        &app,
        &instance,
        json!({
            "id": "first",
            "resource": resource,
            "author": { "id": "alice" },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "text": "Hello" },
        }),
    )
    .await;
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
    let chat = s.chat_list().await.unwrap()[0].clone();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({ "status": "running", "isolated": true }),
        )
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&config["env"], "CAIRN_MCP_RUN_TOKEN");
    assert_eq!(
        agents::call_tool(
            &s,
            bearer,
            "router_scope",
            &json!({ "resource": resource, "channel": "C1" })
        )
        .await
        .unwrap()
        .unwrap()["channel"],
        "C1"
    );
    for input in [
        json!({ "resource": resource, "channel": "C2" }),
        json!({ "resource": other["id"], "channel": "C1" }),
    ] {
        assert!(
            agents::call_tool(&s, bearer, "router_scope", &input)
                .await
                .is_err(),
            "the external run escaped its admission scope: {input}"
        );
    }
    session.stop().await;
}

#[tokio::test]
async fn a_worker_loaded_before_approval_cannot_use_a_newly_reviewed_route() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": { "projects": [], "installation": false, "github": false },
            }),
            None,
        )
        .await
        .unwrap();
    let route = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Channel",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "userIds": ["alice"],
        }),
    )
    .await;
    let old = registry::active(&s, "router").await.unwrap();
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    let mut old_worker = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &old.manifest,
            &old.source,
        )
        .await
        .unwrap();
    let mut manifest = serde_json::to_value(&old.manifest).unwrap();
    manifest["version"] = json!("2");
    let replacement=registry::install(&s,&json!({ "manifest": manifest, "source": format!("-- reviewed replacement\n{}",old.source.main) })).await.unwrap();
    registry::approve(&s, "router", "2", text(&replacement, "digest"))
        .await
        .unwrap();
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}",
        text(&route, "id")
    );
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "PUT",
                &path,
                json!({
                    "name": "Channel",
                    "enabled": true,
                    "match": { "channel": "C1" },
                    "agentId": agent["id"],
                    "userIds": ["alice"],
                    "revision": route["revision"],
                })
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "new-approved-event",
                "resource": resource,
                "author": { "id": "alice" },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "text": "New package only" },
            })
        )
        .await
        .1["status"],
        "accepted"
    );
    let _ = runtime::pump(&s, &connection, &mut old_worker).await;
    assert!(
        s.chat_list().await.unwrap().is_empty(),
        "an old source worker used delegation approved for a different package digest"
    );
    old_worker.stop().await;
}
