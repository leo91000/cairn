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
    configured_fixture(None).await
}

async fn configured_fixture(
    public_origin: Option<&str>,
) -> (TempDir, Arc<Service>, String, String, axum::Router) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".into();
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let mut input = json!({
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
                if event.oauth then
                    local ok, result = pcall(function()
                        local args = {resource = event.resource, provider = 'personal'}
                        if event.oauth == 'start' then args.scopes = {'messages:read'} end
                        if event.overrideOrigin then args.directOrigin = event.overrideOrigin end
                        return cairn.oauth[event.oauth](args)
                    end)
                    local response = cairn.http.request({url = 'https://api.example.test/oauth-result', method = 'POST',
                        json = ok and result or {status = 'refused'}})
                    if response.status >= 500 then error('The remote reply was interrupted') end
                    return response
                end
                if event.question then
                    return cairn.conversations.answer({conversation = event.chat, delivery = event.delivery,
                        question = event.question, answers = {choice = "Yes"}})
                end
                return cairn.conversations.cancel({conversation = event.chat, delivery = event.delivery})
            end
        }"#,
    });
    if public_origin.is_some() {
        input["manifest"]["settings"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "key": "publicOrigin", "title": "Public HTTPS origin", "type": "url"
            }));
        input["manifest"]["oauth"][0]["redirectOriginSetting"] = json!("publicOrigin");
    }
    let package = registry::install(&s, &input).await.unwrap();
    registry::approve(&s, "router", "1", text(&package, "digest"))
        .await
        .unwrap();
    let settings = match public_origin {
        Some(origin) => json!({"clientId": "client-fixture", "publicOrigin": origin}),
        None => json!({"clientId": "client-fixture"}),
    };
    let connection = connections::save(
        &s,
        "router",
        &json!({
            "name": "App",
            "enabled": true,
            "secrets": { "signature": SECRET },
            "settings": settings,
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

async fn oauth_action(
    s: &Service,
    app: &axum::Router,
    connection: &connections::Connection,
    session: &mut cairn_plugin_host::Session,
    input: Value,
    results: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
) -> Value {
    assert_eq!(
        deliver(app, &connection.id, input).await.1["status"],
        "accepted"
    );
    runtime::pump(s, connection, session).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), results.recv())
        .await
        .expect("the plugin did not deliver its personal OAuth result")
        .expect("the OAuth result channel closed")
}

#[tokio::test]
async fn an_admitted_external_person_can_authorize_without_an_installation_account() {
    let (_root, s, instance, resource, app) = fixture().await;
    let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
    let provider = axum::Router::new()
        .route("/token", axum::routing::post(|axum::Form(form): axum::Form<std::collections::HashMap<String, String>>| async move {
            let token = if form["code"] == "native-owner-code" {
                "native-owner-fixture"
            } else {
                "external-person-fixture"
            };
            let workspace = if form["code"] == "other-workspace" { "T222" } else { "T111" };
            let person = if form["code"] == "other-person" { "U222" } else { "U111" };
            axum::Json(json!({
                "access_token": token,
                "scope": "messages:read",
                "team": { "id": workspace },
                "authed_user": { "id": person },
            }))
        }))
        .route("/messages", axum::routing::get(|headers: axum::http::HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer external-person-fixture");
            axum::Json(json!({"subject": "U111"}))
        }))
        .route("/oauth-result", axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
            results.send(value).unwrap();
            async { axum::Json(json!({"ok": true})) }
        }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": {"projects": [], "installation": false, "github": false},
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
            "match": {"channel": "C1"},
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
                json!({
                    "agentId": agent["id"],
                    "profile": "read",
                })
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
    let interaction = |action: &str, person: &str| {
        json!({
            "id": cairn_installation::config::id(),
            "kind": "interaction",
            "resource": resource,
            "author": {"id": person},
            "context": {"channel": "C1", "thread": "thread"},
            "body": {"oauth": action},
        })
    };
    for code in ["other-person", "other-workspace"] {
        let started = oauth_action(
            &s,
            &app,
            &connection,
            &mut session,
            interaction("start", "U111"),
            &mut received,
        )
        .await;
        let url = url::Url::parse(text(&started, "url")).unwrap();
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let callback = format!("/api/plugin-oauth/{instance}/callback?state={state}&code={code}");
        assert_eq!(
            common::send(
                &app,
                common::request("GET", &callback)
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .status(),
            StatusCode::OK
        );
        let refused = oauth_action(
            &s,
            &app,
            &connection,
            &mut session,
            interaction("finish", "U111"),
            &mut received,
        )
        .await;
        assert_eq!(refused["status"], "refused", "accepted proof for {code}");
        let status = oauth_action(
            &s,
            &app,
            &connection,
            &mut session,
            interaction("status", "U111"),
            &mut received,
        )
        .await;
        assert_eq!(status["status"], "disconnected");
    }
    let started = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("start", "U111"),
        &mut received,
    )
    .await;
    assert!(
        started["url"].as_str().is_some(),
        "the admitted external person could not begin OAuth: {started}"
    );
    let url = url::Url::parse(text(&started, "url")).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let callback = format!("/api/plugin-oauth/{instance}/callback?state={state}&code=fixture");
    let callback_response = common::send(
        &app,
        common::request("GET", &callback)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(callback_response.status(), StatusCode::OK);
    assert!(
        !common::read_text(callback_response)
            .await
            .contains("external-person-fixture")
    );
    let stranger = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("finish", "U222"),
        &mut received,
    )
    .await;
    assert_ne!(
        stranger["status"], "authorized",
        "another person finished this authorization"
    );
    let finished = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("finish", "U111"),
        &mut received,
    )
    .await;
    assert_eq!(finished["status"], "authorized", "{finished}");
    assert_eq!(finished["subject"], "U111");
    assert!(!finished.to_string().contains("external-person-fixture"));

    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "authorized-message",
                "resource": resource,
                "author": {"id": "U111"},
                "context": {"channel": "C1", "thread": "thread"},
                "body": {"text": "Read as me"},
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
    let chat = s.chat_list().await.unwrap()[0].clone();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({
                "status": "running",
                "isolated": true,
            }),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    assert_eq!(
        agents::call_tool(&s, bearer, "router_read", &json!({"resource": resource}))
            .await
            .unwrap()
            .unwrap()["subject"],
        "U111"
    );
    let owner_path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/oauth/personal"
    );
    let owner_status =
        common::read_json(common::send(&app, owner_request("GET", &owner_path, Value::Null)).await)
            .await;
    assert_eq!(
        owner_status["status"], "disconnected",
        "the external flow acquired a Cairn account identity"
    );
    // A later explicit Cairn consent may coexist for this same proved person.
    // Disconnecting from the external app must not restore the other credential.
    let native_started = common::read_json(
        common::send(
            &app,
            owner_request("POST", &owner_path, json!({"scopes": ["messages:read"]})),
        )
        .await,
    )
    .await;
    let native_callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=native-owner-code",
        text(&native_started, "state")
    );
    assert_eq!(
        common::send(
            &app,
            common::request("GET", &native_callback)
                .body(Body::empty())
                .unwrap()
        )
        .await
        .status(),
        StatusCode::OK
    );
    let native_finished = common::send(
        &app,
        owner_request(
            "POST",
            &format!("{owner_path}/finish"),
            json!({"state": native_started["state"]}),
        ),
    )
    .await;
    assert_eq!(native_finished.status(), StatusCode::OK);
    assert_eq!(
        agents::call_tool(&s, bearer, "router_read", &json!({"resource": resource}))
            .await
            .unwrap()
            .unwrap()["subject"],
        "U111"
    );
    let disconnected = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("disconnect", "U111"),
        &mut received,
    )
    .await;
    assert_eq!(disconnected["status"], "disconnected");
    assert!(
        agents::call_tool(&s, bearer, "router_read", &json!({"resource": resource}))
            .await
            .is_err()
    );
    let native_status =
        common::read_json(common::send(&app, owner_request("GET", &owner_path, Value::Null)).await)
            .await;
    assert_eq!(native_status["status"], "disconnected");
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn external_oauth_uses_only_the_owner_configured_https_callback_origin() {
    for (origin, expected) in [
        ("https://cairn.example.test", true),
        ("https://cairn.example.test/unrelated", false),
    ] {
        let (_root, s, instance, resource, app) = configured_fixture(Some(origin)).await;
        let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
        let upstream = axum::Router::new().route(
            "/oauth-result",
            axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
                results.send(value).unwrap();
                async { axum::Json(json!({"ok": true})) }
            }),
        );
        let (upstream, server) = common::serve_locally(upstream).await;
        s.plugins.route_http_to(Some(upstream));
        let agent = s
            .agent(
                json!({
                    "name": "Scoped",
                    "access": {"projects": [], "installation": false, "github": false},
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
                "match": {"channel": "C1"},
                "agentId": agent["id"],
                "userIds": ["U111"],
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
        for override_origin in [None, Some("https://untrusted.example.test")] {
            let result = oauth_action(
                &s,
                &app,
                &connection,
                &mut session,
                json!({
                    "id": cairn_installation::config::id(),
                    "kind": "interaction",
                    "resource": resource,
                    "author": {"id": "U111"},
                    "context": {"channel": "C1", "thread": "thread"},
                    "body": {"oauth": "start", "overrideOrigin": override_origin},
                }),
                &mut received,
            )
            .await;
            if expected && override_origin.is_none() {
                assert_eq!(
                    result["redirectUri"],
                    format!("{origin}/api/plugin-oauth/{instance}/callback")
                );
            } else {
                assert_eq!(
                    result["status"], "refused",
                    "accepted an unapproved callback destination"
                );
            }
        }
        session.stop().await;
        server.abort();
    }
}

#[tokio::test]
async fn a_replayed_consent_start_keeps_the_url_the_person_already_received() {
    let (_root, s, instance, resource, app) = fixture().await;
    let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
    let replies = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let upstream = axum::Router::new().route(
        "/oauth-result",
        axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
            results.send(value).unwrap();
            let status = if replies.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            };
            async move { (status, axum::Json(json!({"ok": status == StatusCode::OK}))) }
        }),
    );
    let (upstream, server) = common::serve_locally(upstream).await;
    s.plugins.route_http_to(Some(upstream));
    let agent = s
        .agent(
            json!({
                "name": "Scoped",
                "access": {"projects": [], "installation": false, "github": false},
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
            "match": {"channel": "C1"},
            "agentId": agent["id"],
            "userIds": ["U111"],
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
    let first = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        json!({
            "id": "one-person-action",
            "kind": "interaction",
            "resource": resource,
            "author": {"id": "U111"},
            "context": {"channel": "C1", "thread": "thread"},
            "body": {"oauth": "start"},
        }),
        &mut received,
    )
    .await;
    assert!(first["url"].is_string());
    // Recreate the actual worker, then let the durable event's retry become due.
    session.stop().await;
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
    tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let retried = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retried["url"], first["url"],
        "the retry invalidated an OAuth link already delivered to its person"
    );
    let url = url::Url::parse(text(&first, "url")).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let callback = format!("/api/plugin-oauth/{instance}/callback?state={state}&code=fixture");
    assert_eq!(
        common::send(
            &app,
            common::request("GET", &callback)
                .body(Body::empty())
                .unwrap()
        )
        .await
        .status(),
        StatusCode::OK
    );
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn an_interrupted_external_code_exchange_is_never_replayed() {
    let (_root, s, instance, resource, app) = fixture().await;
    let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let exchanges = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = axum::Router::new()
        .route(
            "/token",
            axum::routing::post({
                let (entered, release, exchanges) =
                    (entered.clone(), release.clone(), exchanges.clone());
                move || {
                    let (entered, release, exchanges) =
                        (entered.clone(), release.clone(), exchanges.clone());
                    async move {
                        exchanges.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        entered.notify_one();
                        release.notified().await;
                        axum::Json(json!({
                            "access_token": "interrupted-fixture",
                            "scope": "messages:read",
                            "team": { "id": "T111" },
                            "authed_user": { "id": "U111" },
                        }))
                    }
                }
            }),
        )
        .route(
            "/oauth-result",
            axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
                results.send(value).unwrap();
                async { axum::Json(json!({ "ok": true })) }
            }),
        );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
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
            "userIds": ["U111"],
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
    let interaction = |action: &str| {
        json!({
            "id": cairn_installation::config::id(),
            "kind": "interaction",
            "resource": resource,
            "author": { "id": "U111" },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "oauth": action },
        })
    };
    let started = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("start"),
        &mut received,
    )
    .await;
    let url = url::Url::parse(text(&started, "url")).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let callback = format!("/api/plugin-oauth/{instance}/callback?state={state}&code=fixture");
    assert_eq!(
        common::send(
            &app,
            common::request("GET", &callback)
                .body(Body::empty())
                .unwrap()
        )
        .await
        .status(),
        StatusCode::OK
    );

    assert_eq!(
        deliver(&app, &instance, interaction("finish")).await.1["status"],
        "accepted"
    );
    let interrupted = tokio::spawn({
        let (s, connection) = (s.clone(), connection.clone());
        async move { runtime::pump(&s, &connection, &mut session).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    // The provider has received the one-use code, but Cairn never gets its result.
    interrupted.abort();
    assert!(interrupted.await.unwrap_err().is_cancelled());
    release.notify_one();

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
    let replay = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay["status"], "refused");
    assert_eq!(exchanges.load(std::sync::atomic::Ordering::SeqCst), 1);
    let status = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("status"),
        &mut received,
    )
    .await;
    assert_eq!(status["status"], "disconnected");
    let restarted = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("start"),
        &mut received,
    )
    .await;
    assert!(
        restarted["url"].is_string(),
        "a new consent could not replace the interrupted exchange"
    );
    assert_ne!(restarted["url"], started["url"]);
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn repeated_question_ids_are_delivered_for_their_own_shared_turn() {
    let (_root, s, instance, resource, app) = fixture().await;
    let original = registry::active(&s, "router").await.unwrap();
    let mut manifest = serde_json::to_value(&original.manifest).unwrap();
    manifest["version"] = json!("2");
    let source = original.source.main.replacen(
        "tool = function(event)",
        r#"
        question = function(event)
            return cairn.http.request({
                url = 'https://api.example.test/question-result', method = 'POST', json = event
            })
        end,
        tool = function(event)"#,
        1,
    );
    let package = registry::install(
        &s,
        &json!({
            "manifest": manifest,
            "source": source,
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "router", "2", text(&package, "digest"))
        .await
        .unwrap();

    let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
    let upstream = axum::Router::new().route(
        "/question-result",
        axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
            results.send(value).unwrap();
            async { axum::Json(json!({ "ok": true })) }
        }),
    );
    let (origin, server) = common::serve_locally(upstream).await;
    s.plugins.route_http_to(Some(origin));
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
            "userIds": ["alice", "bob"],
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
    let question = "b".repeat(64);
    let mut previous_chat = None;
    let mut previous_delivery = None;
    for person in ["alice", "bob"] {
        assert_eq!(
            deliver(
                &app,
                &instance,
                json!({
                    "id": format!("message-{person}"),
                    "resource": resource,
                    "author": { "id": person },
                    "context": { "channel": "C1", "thread": "thread" },
                    "body": { "text": "Continue this thread" },
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
        let chat = s.chat_list().await.unwrap()[0].clone();
        if let Some(previous) = &previous_chat {
            assert_eq!(chat["id"], *previous);
        }
        previous_chat = Some(chat["id"].clone());
        let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
        let run = s
            .store
            .patch_run(
                text(&detail["run"], "id"),
                json!({
                    "status": "running",
                    "isolated": true,
                }),
            )
            .await
            .unwrap();
        let delivery = run["chatExecution"]["messageId"].clone();
        s.chat_acknowledge(text(&run, "id"), text(&run["chatExecution"], "messageId"))
            .await
            .unwrap();
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
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        let notification = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
            .await
            .unwrap_or_else(|_| panic!("the question was hidden in {person}'s turn"))
            .unwrap();
        assert_eq!(notification["deliveryId"], delivery);
        assert_eq!(notification["userId"], person);
        assert_eq!(notification["question"]["id"], question);
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        assert!(
            received.try_recv().is_err(),
            "the same turn received its question twice"
        );

        if let Some(previous) = &previous_delivery {
            assert_eq!(
                deliver(
                    &app,
                    &instance,
                    json!({
                        "id": "old-question-control",
                        "kind": "interaction",
                        "resource": resource,
                        "author": { "id": "alice" },
                        "context": { "channel": "C1", "thread": "thread" },
                        "body": { "chat": chat["id"], "delivery": previous, "question": question },
                    })
                )
                .await
                .1["status"],
                "accepted"
            );
            runtime::pump(&s, &connection, &mut session).await.unwrap();
            let current = s.chat_detail(text(&chat, "id")).await.unwrap();
            assert_eq!(current["questions"][0]["status"], "pending");
            assert_eq!(current["questions"][0]["turnId"], delivery);
        }
        previous_delivery = Some(delivery);
        s.store
            .set(
                &format!("run-checkpoint:{}", text(&run, "id")),
                json!({
                    "completed": true,
                    "launched": true,
                }),
                None,
            )
            .await
            .unwrap();
        s.store
            .patch_run(text(&run, "id"), json!({ "status": "succeeded" }))
            .await
            .unwrap();
        runtime::pump(&s, &connection, &mut session).await.unwrap();
    }
    session.stop().await;
    server.abort();
}
