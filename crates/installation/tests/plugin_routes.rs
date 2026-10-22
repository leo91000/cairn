mod common;
#[path = "plugin_routes/continuations.rs"]
mod continuations;

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
    configured_fixture_with_runner(public_origin, "http://runner:4311").await
}

async fn configured_fixture_with_runner(
    public_origin: Option<&str>,
    runner: &str,
) -> (TempDir, Arc<Service>, String, String, axum::Router) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = runner.to_owned();
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
                return cairn.conversations.submit({text = event.text, thread = event.context.thread, shared = true, newTurn = event.newTurn,
                    reply = {resource = event.resource, route = event.route, channel = event.context.channel, trace = event.trace, holdProgress = event.holdProgress}})
            end,
            progress = function(event)
                if event.reply.holdProgress then
                    return cairn.http.request({url = 'https://api.example.test/progress-result', method = 'POST', json = event})
                end
            end,
            reply = function(event)
                if event.reply.trace then
                    return cairn.http.request({url = 'https://api.example.test/reply-result', method = 'POST', json = event})
                end
            end,
            interaction = function(event)
                if event.reset then
                    local ok, result = pcall(function()
                        return cairn.conversations.reset({conversation = event.chat, thread = event.context.thread, shared = true})
                    end)
                    local response = cairn.http.request({url = 'https://api.example.test/reset-result', method = 'POST',
                        json = ok and result or {status = 'refused'}})
                    if response.status >= 500 then error('The reset acknowledgement was interrupted') end
                    return response
                end
                if event.stopDelay then
                    local result = {}
                    if event.stopDelay ~= 'wait' then
                        result = cairn.conversations.cancel({conversation = event.chat, delivery = event.delivery})
                    end
                    cairn.http.request({url = 'https://api.example.test/stop-barrier'})
                    return result
                end
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
async fn route_summary_pages_report_only_their_own_usage_and_pending_people() {
    async fn pages(app: &axum::Router, path: &str) -> (usize, Vec<Value>) {
        let mut cursor = None;
        let mut rows = Vec::new();
        let mut pages = 0;
        loop {
            let url = cursor
                .as_ref()
                .map_or_else(|| path.to_owned(), |value| format!("{path}?cursor={value}"));
            let response = common::send(app, owner_request("GET", &url, Value::Null)).await;
            assert_eq!(response.status(), StatusCode::OK);
            let page = common::read_json(response).await;
            let items = page["items"].as_array().unwrap();
            assert!(items.len() <= 32);
            rows.extend(items.iter().cloned());
            pages += 1;
            cursor = page["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            assert!(pages < 3, "route pagination did not terminate");
        }
        (pages, rows)
    }
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Summary helper",
                "access": {
                    "projects": [],
                    "installation": false,
                    "github": false,
                },
            }),
            None,
        )
        .await
        .unwrap();
    let path =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes");
    let mut saved = Vec::new();
    for index in 0..35 {
        saved.push(
            create_route(
                &app,
                &instance,
                &resource,
                json!({
                    "name": format!("Channel {index}"),
                    "enabled": true,
                    "match": { "channel": format!("C{index}") },
                    "agentId": agent["id"],
                    "accessMode": "requests",
                    "maxMessagesPerHour": 100,
                }),
            )
            .await,
        );
    }
    // Existing rate records, including the next hour, keep this read-model test
    // deterministic across an hour boundary. The root has a different counter.
    let rates = saved.clone();
    let root_id = instance.clone();
    s.store
        .transaction(move |db| {
            let hour = now() / 3_600_000;
            for period in [hour - 1, hour, hour + 1] {
                db.set(&format!("plugin-rate:{root_id}:{period}"), &json!(99), None)?;
                for (index, route) in rates.iter().enumerate() {
                    let count = if period == hour - 1 { 90 } else { index + 1 };
                    db.set(
                        &format!("plugin-rate:{}:{period}", text(route, "id")),
                        &json!(count),
                        None,
                    )?;
                }
            }
            Ok(())
        })
        .await
        .unwrap();
    for (id, channel, author) in [
        ("first", "C0", "bob"),
        ("again", "C0", "bob"),
        ("other", "C1", "jane"),
    ] {
        let (status, result) = deliver(
            &app,
            &instance,
            json!({
                "id": id,
                "resource": resource,
                "author": { "id": author },
                "context": {
                    "channel": channel,
                    "thread": id,
                },
                "body": { "text": "not admitted" },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["status"], "requested");
    }
    let before_read = now() / 3_600_000 * 3_600_000;
    let (page_count, rows) = pages(&app, &path).await;
    let after_read = now() / 3_600_000 * 3_600_000;
    assert_eq!(page_count, 2);
    assert_eq!(rows.len(), saved.len());
    for (index, original) in saved.iter().enumerate() {
        let row = rows.iter().find(|row| row["id"] == original["id"]).unwrap();
        assert_eq!(row["summary"]["messagesThisHour"], index + 1, "{row}");
        assert_eq!(
            row["summary"]["pendingRequests"],
            usize::from(index < 2),
            "{row}"
        );
        assert!(
            [before_read, after_read].contains(&row["summary"]["hourStartedAt"].as_i64().unwrap())
        );
        assert_eq!(
            row["revision"], original["revision"],
            "volatile summaries changed the edit revision"
        );
    }
    let record_id = text(&saved[0], "id").to_owned();
    let stored = s
        .store
        .read(move |db| db.get("plugin-routes", &record_id))
        .await
        .unwrap()
        .unwrap();
    assert!(
        stored.get("summary").is_none(),
        "summaries must not become persisted route state"
    );

    let first = &saved[0];
    let first_path = format!("{path}/{}", text(first, "id"));
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &format!("{first_path}/people/bob"),
            json!({
                "action": "deny",
                "revision": first["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let denied = common::read_json(response).await;
    let response = common::send(
        &app,
        owner_request(
            "PUT",
            &first_path,
            json!({
                "name": "Paused summary",
                "enabled": false,
                "match": { "channel": "C0" },
                "agentId": agent["id"],
                "accessMode": "requests",
                "revision": denied["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let second_path = format!("{path}/{}", text(&saved[1], "id"));
    let response = common::send(
        &app,
        owner_request(
            "DELETE",
            &second_path,
            json!({
                "revision": saved[1]["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let (_, updated) = pages(&app, &path).await;
    assert_eq!(updated.len(), saved.len() - 1);
    assert!(updated.iter().all(|row| row["id"] != saved[1]["id"]));
    let row = updated.iter().find(|row| row["id"] == first["id"]).unwrap();
    assert_eq!(row["summary"]["pendingRequests"], 0);
    assert_eq!(row["summary"]["messagesThisHour"], 1);
    assert_eq!(row["enabled"], false);
    let mut request = owner_request("GET", &path, Value::Null);
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Member,
            "member",
        ));
    assert_eq!(
        common::send(&app, request).await.status(),
        StatusCode::FORBIDDEN
    );
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
        common::send(
            &app,
            owner_request(
                "DELETE",
                &route_path,
                json!({"revision": blocked["revision"]})
            )
        )
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
async fn an_ambiguous_route_retires_only_its_pending_delivery() {
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let platform = axum::Router::new().route(
        "/reply-result",
        axum::routing::post(move |axum::Json(reply): axum::Json<Value>| {
            sent.send(reply).unwrap();
            async { axum::Json(json!({ "ok": true })) }
        }),
    );
    let (origin, server) = common::serve_locally(platform).await;
    let (_root, s, instance, resource, app) = fixture().await;
    s.plugins.route_http_to(Some(origin));
    let agent = s.agent(json!({
        "name": "Scoped", "access": { "projects": [], "installation": false, "github": false },
    }), None).await.unwrap();
    for channel in ["C1", "C2"] {
        create_route(
            &app,
            &instance,
            &resource,
            json!({
                "name": channel, "enabled": true, "match": { "channel": channel },
                "agentId": agent["id"], "userIds": ["alice"],
            }),
        )
        .await;
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
    let request = |channel: &str, id: &str| {
        json!({
            "id": id, "resource": resource, "author": { "id": "alice" },
            "context": { "channel": channel, "kind": channel, "thread": channel },
            "body": { "text": channel, "trace": true },
        })
    };
    for channel in ["C2", "C1"] {
        assert_eq!(
            deliver(&app, &instance, request(channel, channel)).await.1["status"],
            "accepted"
        );
        runtime::pump(&s, &connection, &mut session).await.unwrap();
    }
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let deliveries = s.store.list("plugin-deliveries").await.unwrap();
    assert_eq!(deliveries.len(), 2);
    for delivery in &deliveries {
        let chat = s.get("chats", text(delivery, "chatId")).await.unwrap();
        s.store
            .patch_run(
                text(&chat, "runId"),
                json!({
                    "status": "cancelled", "recoveryPending": false,
                }),
            )
            .await
            .unwrap();
    }
    let blocked = deliveries
        .iter()
        .find(|value| value["reply"]["channel"] == "C1")
        .unwrap();
    let blocked_id = text(blocked, "id").to_owned();
    // Exercise the adverse queue order deterministically, without a timing race.
    s.store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE records SET updated_at=?1 WHERE kind='plugin-deliveries' AND id=?2",
                rusqlite::params![now() + 1000, blocked_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        s.store.list("plugin-deliveries").await.unwrap()[0]["id"],
        blocked["id"]
    );
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Overlapping C1", "enabled": true, "match": { "kind": "C1" },
            "agentId": agent["id"], "userIds": ["alice"],
        }),
    )
    .await;
    assert_eq!(
        deliver(&app, &instance, request("C1", "new-ambiguous"))
            .await
            .0,
        StatusCode::CONFLICT
    );

    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(
        session.alive(),
        "A route conflict stopped the shared worker"
    );
    assert!(s.store.list("plugin-deliveries").await.unwrap().is_empty());
    let reply = received
        .try_recv()
        .expect("The unaffected route's reply was stranded");
    assert_eq!(reply["reply"]["channel"], "C2");
    assert_eq!(reply["status"], "cancelled");
    assert!(
        received.try_recv().is_err(),
        "A revoked route published a reply"
    );
    let activity = s.store.list("plugin-activity").await.unwrap();
    for delivery in deliveries {
        let completed = activity
            .iter()
            .find(|entry| entry["deliveryId"] == delivery["id"])
            .unwrap();
        assert_eq!(
            completed["status"],
            if delivery["reply"]["channel"] == "C1" {
                "failed"
            } else {
                "stopped"
            }
        );
    }
    session.stop().await;
    server.abort();
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
    let mut route_revisions = Vec::new();
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
        route_revisions.push(route["revision"].clone());
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
        owner_request(
            "POST",
            &path,
            json!({ "action": "approve", "revision": route_revisions[0] }),
        ),
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
    queued_control_after_source_change("person").await;
}

#[tokio::test]
async fn queued_question_answers_recheck_routes_packages_and_owner_authority() {
    for change in ["pause", "delete", "package", "owner"] {
        queued_control_after_source_change(change).await;
    }
}

async fn queued_control_after_source_change(change: &str) {
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
                "accessMode": "requests",
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
    let mut question = "a".repeat(64);
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
    let first_answer = detail["questions"][0]["messageId"].clone();
    let inputs_path = s
        .config
        .data_dir
        .join("runs")
        .join(text(&run, "id"))
        .join("chat-input/messages.json");
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inputs: Value = serde_json::from_slice(&std::fs::read(&inputs_path).unwrap()).unwrap();
    assert!(
        inputs
            .as_array()
            .unwrap()
            .iter()
            .any(|input| input["id"] == first_answer),
        "The positive control did not deliver the current author's answer"
    );
    s.chat_acknowledge(text(&run, "id"), first_answer.as_str().unwrap())
        .await
        .unwrap();
    let completed_question = question;
    question = "b".repeat(64);
    s.question_receive(
        text(&run, "id"),
        json!({
            "id": question,
            "blocking": false,
            "fields": [{ "id": "choice", "title": "Continue again?", "options": [] }],
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "own-answer-to-revoke",
                "kind": "interaction",
                "resource": resource,
                "author": { "id": "alice" },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "chat": chat_id, "delivery": message, "question": question },
            })
        )
        .await
        .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let detail = s.chat_detail(chat_id).await.unwrap();
    let pending = detail["questions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == question)
        .unwrap();
    assert_eq!(pending["status"], "answering");
    let queued_answer = pending["messageId"].clone();
    session.stop().await;

    let routes =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes");
    let response = common::send(&app, owner_request("GET", &routes, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let listed = common::read_json(response).await;
    let route = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["match"]["channel"] == "C1")
        .unwrap();
    let mut authority_server = None;
    if change == "person" {
        let person = format!("{routes}/{}/people/alice", text(route, "id"));
        let response = common::send(
            &app,
            owner_request(
                "POST",
                &person,
                json!({
                    "action": "remove",
                    "revision": route["revision"],
                }),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let removed = common::read_json(response).await;
        assert_eq!(
            deliver(
                &app,
                &instance,
                json!({
                    "id": "fresh-access-request",
                    "resource": resource,
                    "author": { "id": "alice" },
                    "context": { "channel": "C1", "thread": "request" },
                    "body": { "text": "Request access again" },
                })
            )
            .await
            .1["status"],
            "requested"
        );
        let response = common::send(
            &app,
            owner_request(
                "POST",
                &person,
                json!({
                    "action": "approve",
                    "revision": removed["revision"],
                }),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    } else if change == "pause" || change == "delete" {
        let path = format!("{routes}/{}", text(route, "id"));
        let (method, body) = if change == "delete" {
            ("DELETE", json!({ "revision": route["revision"] }))
        } else {
            (
                "PUT",
                json!({
                    "name": "C1",
                    "enabled": false,
                    "match": { "channel": "C1" },
                    "agentId": agent["id"],
                    "accessMode": "requests",
                    "userIds": ["alice", "bob"],
                    "revision": route["revision"],
                }),
            )
        };
        let response = common::send(&app, owner_request(method, &path, body)).await;
        assert_eq!(response.status(), StatusCode::OK, "{change}");
    } else if change == "package" {
        let mut manifest = serde_json::to_value(&package.manifest).unwrap();
        manifest["version"] = json!("2");
        let replacement = registry::install(
            &s,
            &json!({
                "manifest": manifest,
                "source": format!("-- new approval\n{}", package.source.main),
            }),
        )
        .await
        .unwrap();
        registry::approve(&s, "router", "2", text(&replacement, "digest"))
            .await
            .unwrap();
    } else if change == "owner" {
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(|| async {
                axum::Json(json!({
                    "owner": {
                        "account_id": "fixture-owner",
                        "access_id": "owner-new-generation",
                    },
                    "members": [],
                }))
            }),
        );
        let (origin, server) = common::serve_locally(authority).await;
        let path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
        authority_server = Some(server);
    } else {
        panic!("Unknown authority change: {change}");
    }

    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inputs: Value = serde_json::from_slice(
        &std::fs::read(
            s.config
                .data_dir
                .join("runs")
                .join(text(&run, "id"))
                .join("chat-input/messages.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        inputs
            .as_array()
            .unwrap()
            .iter()
            .all(|input| input["id"] != queued_answer),
        "A retired source answer reached the running native process"
    );
    let detail = s.chat_detail(chat_id).await.unwrap();
    let answer = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|answer| answer["id"] == queued_answer)
        .unwrap();
    assert_eq!(answer["status"], "cancelled");
    let pending = detail["questions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == question)
        .unwrap();
    assert_eq!(
        pending["status"], "pending",
        "The owner must still be able to answer after a revoked control is refused: {change}"
    );
    assert_eq!(
        detail["questions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == completed_question)
            .unwrap()["status"],
        "answered"
    );

    let owner_answer = cairn_installation::config::id();
    let path = format!("/api/chats/{chat_id}/questions/{question}/answer");
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &path,
            json!({
                "id": owner_answer,
                "answers": { "choice": ["Owner continues"] },
            }),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Owner rescue after {change}"
    );
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inputs: Value = serde_json::from_slice(&std::fs::read(&inputs_path).unwrap()).unwrap();
    assert!(
        inputs
            .as_array()
            .unwrap()
            .iter()
            .any(|input| input["id"] == owner_answer),
        "Owner control was blocked by the retired external source: {change}"
    );
    assert!(
        inputs
            .as_array()
            .unwrap()
            .iter()
            .all(|input| input["id"] != queued_answer)
    );
    s.chat_acknowledge(text(&run, "id"), &owner_answer)
        .await
        .unwrap();
    let detail = s.chat_detail(chat_id).await.unwrap();
    assert_eq!(
        detail["questions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == question)
            .unwrap()["status"],
        "answered"
    );
    if let Some(server) = authority_server {
        server.abort();
    }
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
            let continued = wake_after_sleep(
                &s,
                text(chat, "id"),
                &run,
                text(&config["env"], "CAIRN_MCP_RUN_TOKEN"),
            )
            .await;
            let config = s.mcps.run_configuration(&s, &continued).await.unwrap();
            let result = agents::call_tool(
                &s,
                text(&config["env"], "CAIRN_MCP_RUN_TOKEN"),
                "router_read",
                &json!({"resource": resource}),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                result["subject"], "U111",
                "a durable wake lost its personal OAuth requester"
            );
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

// Native lifecycle fixture: actual sleep API and scheduler, with a prepared
// completed checkpoint instead of a launched process/VM.
async fn wake_after_sleep(s: &Service, chat: &str, run: &Value, bearer: &str) -> Value {
    let run_id = text(run, "id");
    s.chat_acknowledge(run_id, text(&run["chatExecution"], "messageId"))
        .await
        .unwrap();
    cairn_installation::chats::sleep::schedule(
        s,
        bearer,
        &json!({
            "seconds": 1,
            "reason": "Await a provider response as the same requester",
        }),
    )
    .await
    .unwrap();
    let checkpoint = json!({"launched": true, "completed": true, "remainingMs": null});
    s.store
        .set(&format!("run-checkpoint:{run_id}"), checkpoint, None)
        .await
        .unwrap();
    s.store
        .event(run_id, "turn.completed", "", None)
        .await
        .unwrap();
    s.store
        .patch_run(
            run_id,
            json!({ "status": "succeeded", "recoveryPending": false }),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let continued = s.store.run(run_id).await.unwrap();
    let waiting_chat = s.chat_detail(chat).await.unwrap();
    assert_ne!(
        continued["chatExecution"]["messageId"],
        run["chatExecution"]["messageId"],
        "wakeup={}, paused={}, error={:?}",
        waiting_chat["wakeup"],
        waiting_chat["paused"],
        s.store.kv(&format!("chat-error:{chat}")).await.unwrap()
    );
    s.store
        .patch_run(
            run_id,
            json!({
                "status": "running",
                "recoveryPending": false,
                "isolated": true,
            }),
        )
        .await
        .unwrap()
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
    let continued = wake_after_sleep(&s, text(&chat, "id"), &run, bearer).await;
    let configuration = s.mcps.run_configuration(&s, &continued).await.unwrap();
    let continued_bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    assert_eq!(
        agents::call_tool(
            &s,
            continued_bearer,
            "router_scope",
            &json!({
                "resource": resource, "channel": "C1",
            })
        )
        .await
        .unwrap()
        .unwrap()["channel"],
        "C1"
    );
    for input in [
        json!({"resource": resource, "channel": "C2"}),
        json!({"resource": other["id"], "channel": "C1"}),
    ] {
        assert!(
            agents::call_tool(&s, continued_bearer, "router_scope", &input)
                .await
                .is_err(),
            "the durable continuation escaped its requester's source scope: {input}"
        );
    }
    let question = "b".repeat(64);
    s.question_receive(
        text(&continued, "id"),
        json!({
            "id": question,
            "blocking": false,
            "fields": [{ "id": "choice", "title": "Continue?", "options": [] }],
        }),
    )
    .await
    .unwrap();
    let answer_path = format!(
        "/api/chats/{}/questions/{question}/answer",
        text(&chat, "id")
    );
    let answer = json!({
        "id": cairn_installation::config::id(),
        "answers": { "choice": ["Yes"] },
    });
    let response = common::send(&app, owner_request("POST", &answer_path, answer.clone())).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "owner control was denied"
    );
    assert_eq!(
        agents::call_tool(
            &s,
            continued_bearer,
            "router_scope",
            &json!({
                "resource": resource, "channel": "C1",
            })
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
            agents::call_tool(&s, continued_bearer, "router_scope", &input)
                .await
                .is_err(),
            "owner intervention released the requester's source scope: {input}"
        );
    }
    s.chat_acknowledge(text(&continued, "id"), text(&answer, "id"))
        .await
        .unwrap();
    let resumed = wake_after_sleep(&s, text(&chat, "id"), &continued, continued_bearer).await;
    let configuration = s.mcps.run_configuration(&s, &resumed).await.unwrap();
    let resumed_bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    assert_eq!(
        agents::call_tool(
            &s,
            resumed_bearer,
            "router_scope",
            &json!({
                "resource": resource, "channel": "C1",
            })
        )
        .await
        .unwrap()
        .unwrap()["channel"],
        "C1"
    );
    assert!(
        agents::call_tool(
            &s,
            resumed_bearer,
            "router_scope",
            &json!({
                "resource": resource, "channel": "C2",
            })
        )
        .await
        .is_err(),
        "a second wake released the suspended requester's source scope"
    );
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

async fn assert_consent_replay(interleave: bool, remove_instance: bool) {
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
    assert_eq!(s.store.keys("plugin-oauth-event:").await.unwrap().len(), 1);
    if remove_instance {
        let path = format!("/api/plugins/router/connections/{instance}");
        let response = common::send(&app, owner_request("DELETE", &path, Value::Null)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            s.store
                .keys("plugin-oauth-event:")
                .await
                .unwrap()
                .is_empty()
        );
        session.stop().await;
        server.abort();
        return;
    }

    let latest = if interleave {
        oauth_action(
            &s,
            &app,
            &connection,
            &mut session,
            json!({
                "id": "second-person-action",
                "kind": "interaction",
                "resource": resource,
                "author": {"id": "U111"},
                "context": {"channel": "C1", "thread": "thread"},
                "body": {"oauth": "start"},
            }),
            &mut received,
        )
        .await
    } else {
        first.clone()
    };
    assert!(latest["url"].is_string());
    assert_eq!(s.store.keys("plugin-oauth-event:").await.unwrap().len(), 1);
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
    if !interleave {
        assert_eq!(
            retried["url"], first["url"],
            "the retry invalidated an OAuth link already delivered to its person"
        );
    }
    let url = url::Url::parse(text(&latest, "url")).unwrap();
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
        StatusCode::OK,
        "Replaying an older consent invalidated the latest delivered link"
    );
    if interleave {
        assert_eq!(
            retried["status"], "refused",
            "A superseded event started a new consent"
        );
        assert!(retried["url"].is_null());
    }
    assert!(
        s.store
            .keys("plugin-oauth-event:")
            .await
            .unwrap()
            .is_empty()
    );
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn a_replayed_consent_start_keeps_the_url_the_person_already_received() {
    assert_consent_replay(false, false).await;
}

#[tokio::test]
async fn an_old_consent_retry_cannot_replace_a_newer_person_request() {
    assert_consent_replay(true, false).await;
}

#[tokio::test]
async fn removing_an_instance_retires_its_pending_event_consent_receipts() {
    assert_consent_replay(false, true).await;
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

async fn install_question_handler(s: &Arc<Service>) {
    let original = registry::active(s, "router").await.unwrap();
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
        s,
        &json!({
            "manifest": manifest,
            "source": source,
        }),
    )
    .await
    .unwrap();
    registry::approve(s, "router", "2", text(&package, "digest"))
        .await
        .unwrap();
}

#[tokio::test]
async fn repeated_question_ids_are_delivered_for_their_own_shared_turn() {
    let (_root, s, instance, resource, app) = fixture().await;
    install_question_handler(&s).await;

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

#[tokio::test]
async fn a_queued_new_turn_can_follow_a_stopped_author_without_inheriting_their_identity() {
    let (_root, s, instance, resource, app) = fixture().await;
    let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
    let upstream = axum::Router::new()
        .route("/token", axum::routing::post(|axum::Form(form): axum::Form<std::collections::HashMap<String, String>>| async move {
            let (token, subject) = match form["code"].as_str() {
                "alice" => ("alice-turn-fixture", "U111"),
                "bob" => ("bob-turn-fixture", "U222"),
                _ => panic!("unexpected fixture consent"),
            };
            axum::Json(json!({
                "access_token": token,
                "scope": "messages:read",
                "team": { "id": "T111" },
                "authed_user": { "id": subject },
            }))
        }))
        .route("/messages", axum::routing::get(|headers: axum::http::HeaderMap| async move {
            let subject = match headers["authorization"].to_str().unwrap() {
                "Bearer alice-turn-fixture" => "U111",
                "Bearer bob-turn-fixture" => "U222",
                _ => panic!("unexpected requester credential"),
            };
            axum::Json(json!({ "subject": subject }))
        }))
        .route("/oauth-result", axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
            results.send(value).unwrap();
            async { axum::Json(json!({ "ok": true })) }
        }));
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
    for (person, code) in [("U111", "alice"), ("U222", "bob")] {
        let action = |method: &str| {
            json!({
                "id": cairn_installation::config::id(),
                "kind": "interaction",
                "resource": resource,
                "author": { "id": person },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "oauth": method },
            })
        };
        let started = oauth_action(
            &s,
            &app,
            &connection,
            &mut session,
            action("start"),
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
        let finished = oauth_action(
            &s,
            &app,
            &connection,
            &mut session,
            action("finish"),
            &mut received,
        )
        .await;
        assert_eq!(finished["status"], "authorized");
    }
    let message = |id: &str, person: &str, fresh: bool| {
        json!({
            "id": id,
            "resource": resource,
            "author": { "id": person },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "text": format!("Request from {person}"), "newTurn": fresh },
        })
    };
    assert_eq!(
        deliver(&app, &instance, message("old-request", "U111", false))
            .await
            .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.chat_list().await.unwrap()[0].clone();
    let run = s.chat_detail(text(&chat, "id")).await.unwrap()["run"].clone();
    let run_id = text(&run, "id");
    let old_delivery = run["chatExecution"]["messageId"].clone();
    s.store
        .patch_run(
            run_id,
            json!({
                "status": "running",
                "isolated": true,
                "sessionId": "old-native-session",
            }),
        )
        .await
        .unwrap();
    let run = s.store.run(run_id).await.unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let old_bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    assert_eq!(
        agents::call_tool(
            &s,
            old_bearer,
            "router_read",
            &json!({ "resource": resource })
        )
        .await
        .unwrap()
        .unwrap()["subject"],
        "U111"
    );

    let question = "c".repeat(64);
    s.question_receive(
        run_id,
        json!({
            "id": question,
            "blocking": false,
            "fields": [{ "id": "choice", "title": "Continue?", "options": [] }],
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "answer-before-stop",
                "kind": "interaction",
                "resource": resource,
                "author": { "id": "U111" },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "chat": chat["id"], "delivery": old_delivery, "question": question },
            }),
        )
        .await
        .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let answered = s.chat_detail(text(&chat, "id")).await.unwrap();
    assert_eq!(answered["questions"][0]["status"], "answering");
    let pending_answer = answered["questions"][0]["messageId"].clone();

    assert_eq!(
        deliver(&app, &instance, message("new-request", "U222", true))
            .await
            .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        s.store.run(run_id).await.unwrap()["chatExecution"]["messageId"],
        old_delivery
    );
    // Model a saved, settled workspace; this test does not launch an agent VM.
    s.store
        .set(
            &format!("run-checkpoint:{run_id}"),
            json!({
                "completed": true,
                "launched": true,
            }),
            None,
        )
        .await
        .unwrap();
    let stop = |id: &str, person: &str, delivery: &Value| {
        json!({
            "id": id,
            "kind": "interaction",
            "resource": resource,
            "author": { "id": person },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "chat": chat["id"], "delivery": delivery },
        })
    };
    assert_eq!(
        deliver(&app, &instance, stop("stop-old", "U111", &old_delivery))
            .await
            .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let overview = common::send(&app, owner_request("GET", "/api/plugins", Value::Null)).await;
    assert_eq!(overview.status(), StatusCode::OK);
    let overview = common::read_json(overview).await;
    let activity = &overview["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == instance)
        .unwrap()["activity"];
    assert_eq!(
        activity
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["userId"] == "U222")
            .unwrap()["status"],
        "working",
        "the queued request's delivery was cancelled with its predecessor"
    );
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let next = s.chat_detail(text(&chat, "id")).await.unwrap();
    assert_eq!(
        next["run"]["chatExecution"]["text"], "Request from U222",
        "the queued requester could not start a new turn after the earlier author stopped"
    );
    assert_eq!(next["run"]["id"], run["id"]);
    assert!(
        next["run"]["sessionId"].is_null(),
        "the old native session was resumed for a different requester"
    );
    assert_eq!(s.chat_list().await.unwrap().len(), 1);
    assert_eq!(next["questions"][0]["status"], "cancelled");
    assert!(
        next["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| { message["id"] == pending_answer && message["status"] == "cancelled" }),
        "an old answer was retained for the next requester's session"
    );
    assert!(
        next["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["id"] == old_delivery && message["status"] == "cancelled")
    );
    s.store
        .patch_run(run_id, json!({ "status": "running" }))
        .await
        .unwrap();
    let current = s.store.run(run_id).await.unwrap();
    let configuration = s.mcps.run_configuration(&s, &current).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    assert!(
        agents::call_tool(
            &s,
            old_bearer,
            "router_read",
            &json!({ "resource": resource })
        )
        .await
        .is_err()
    );
    assert_eq!(
        agents::call_tool(&s, bearer, "router_read", &json!({ "resource": resource }))
            .await
            .unwrap()
            .unwrap()["subject"],
        "U222"
    );
    assert_eq!(
        deliver(&app, &instance, stop("stale-stop", "U111", &old_delivery))
            .await
            .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(s.store.run(run_id).await.unwrap()["status"], "running");
    assert_eq!(
        deliver(&app, &instance, message("new-request", "U222", true))
            .await
            .1["status"],
        "duplicate"
    );
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn pending_new_turns_respect_owner_pause_and_workspace_settlement() {
    pending_new_turn_scenarios(&[
        "owner-pause",
        "owner-pause-before-enqueue",
        "unsettled-workspace",
        "owner-pause-after-success",
        "owner-pause-before-enqueue-after-success",
        "unsettled-workspace-after-success",
    ])
    .await;
}

#[tokio::test]
async fn queued_new_turns_retry_temporary_authority_outages_without_owner_resume() {
    pending_new_turn_scenarios(&["authority-unavailable"]).await;
}

#[tokio::test]
async fn queued_new_turns_retire_revoked_people_and_preserve_other_participants() {
    pending_new_turn_scenarios(&["revoked-person"]).await;
}

#[tokio::test]
async fn queued_new_turns_recheck_participants_after_slow_authority_lookup() {
    pending_new_turn_scenarios(&[
        "revoked-during-lookup",
        "revoked-during-lookup-after-success",
    ])
    .await;
}

#[tokio::test]
async fn queued_new_turns_retire_revoked_people_after_a_successful_turn() {
    pending_new_turn_scenarios(&["revoked-person-after-success"]).await;
}

#[tokio::test]
async fn queued_new_turns_retry_authority_outages_after_a_successful_turn() {
    pending_new_turn_scenarios(&["authority-unavailable-after-success"]).await;
}

async fn pending_new_turn_scenarios(scenarios: &[&str]) {
    for &case in scenarios {
        let succeeded = case.ends_with("-after-success");
        let scenario = case.strip_suffix("-after-success").unwrap_or(case);
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
                "userIds": ["alice", "bob", "carol"],
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
        let message = |person: &str, fresh: bool| {
            json!({
                "id": format!("request-{person}"),
                "resource": resource,
                "author": { "id": person },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "text": format!("Request from {person}"), "newTurn": fresh },
            })
        };
        assert_eq!(
            deliver(&app, &instance, message("alice", false)).await.1["status"],
            "accepted"
        );
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        s.chat_tick(&std::collections::HashSet::new())
            .await
            .unwrap();
        let chat = s.chat_list().await.unwrap()[0].clone();
        let run = s.chat_detail(text(&chat, "id")).await.unwrap()["run"].clone();
        let run_id = text(&run, "id");
        s.store
            .patch_run(run_id, json!({ "status": "running", "isolated": true }))
            .await
            .unwrap();
        s.store
            .set(
                &format!("run-checkpoint:{run_id}"),
                json!({
                    "completed": scenario != "unsettled-workspace",
                    "launched": true,
                }),
                None,
            )
            .await
            .unwrap();
        if scenario == "owner-pause-before-enqueue" {
            let paused = common::send(
                &app,
                owner_request(
                    "POST",
                    &format!("/api/chats/{}/pause", text(&chat, "id")),
                    json!({ "paused": true }),
                ),
            )
            .await;
            assert_eq!(paused.status(), StatusCode::OK);
        }
        assert_eq!(
            deliver(&app, &instance, message("bob", true)).await.1["status"],
            "accepted"
        );
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        if scenario.starts_with("revoked-") {
            assert_eq!(
                deliver(&app, &instance, message("carol", true)).await.1["status"],
                "accepted"
            );
            runtime::pump(&s, &connection, &mut session).await.unwrap();
        }
        if scenario == "owner-pause" {
            let paused = common::send(
                &app,
                owner_request(
                    "POST",
                    &format!("/api/chats/{}/pause", text(&chat, "id")),
                    json!({ "paused": true }),
                ),
            )
            .await;
            assert_eq!(paused.status(), StatusCode::OK);
        } else if scenario == "revoked-person" {
            let path = format!(
                "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/bob",
                text(&route, "id")
            );
            let removed = common::send(
                &app,
                owner_request(
                    "POST",
                    &path,
                    json!({ "action": "remove", "revision": route["revision"] }),
                ),
            )
            .await;
            assert_eq!(removed.status(), StatusCode::OK);
        }
        if succeeded {
            s.chat_acknowledge(run_id, text(&run["chatExecution"], "messageId"))
                .await
                .unwrap();
            s.store
                .event(
                    run_id,
                    "item.completed",
                    "",
                    Some(json!({ "item": { "type": "agent_message", "text": "Alice's answer" } })),
                )
                .await
                .unwrap();
            s.store
                .event(run_id, "turn.completed", "", None)
                .await
                .unwrap();
            s.store
                .patch_run(run_id, json!({ "status": "succeeded" }))
                .await
                .unwrap();
        } else {
            assert_eq!(
                deliver(
                    &app,
                    &instance,
                    json!({
                        "id": "stop-alice",
                        "kind": "interaction",
                        "resource": resource,
                        "author": { "id": "alice" },
                        "context": { "channel": "C1", "thread": "thread" },
                        "body": { "chat": chat["id"], "delivery": run["chatExecution"]["messageId"] },
                    })
                )
                .await
                .1["status"],
                "accepted"
            );
            runtime::pump(&s, &connection, &mut session).await.unwrap();
        }
        if scenario == "unsettled-workspace" {
            s.store
                .patch_run(run_id, json!({ "recoveryPending": true }))
                .await
                .unwrap();
        }
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let authority_server = if scenario == "authority-unavailable"
            || scenario == "revoked-during-lookup"
        {
            let delayed = scenario == "revoked-during-lookup";
            let entering = entered.clone();
            let releasing = release.clone();
            let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let authority = axum::Router::new().route(
                "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
                axum::routing::get(move || {
                    let first = first.swap(false, std::sync::atomic::Ordering::SeqCst);
                    let (entered, release) = (entering.clone(), releasing.clone());
                    async move {
                        if first && delayed {
                            entered.notify_one();
                            release.notified().await;
                        }
                        if first && !delayed {
                            (StatusCode::SERVICE_UNAVAILABLE, "Synthetic authority unavailable").into_response()
                        } else {
                            axum::Json(json!({
                                "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                                "members": [],
                            })).into_response()
                        }
                    }
                }),
            );
            let (origin, server) = common::serve_locally(authority).await;
            let identity_path = s.config.data_dir.join("installation-relay/identity.json");
            let mut identity: Value =
                serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
            identity["origin"] = json!(origin);
            std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
            Some(server)
        } else {
            None
        };
        let service = s.clone();
        let ticking =
            tokio::spawn(async move { service.chat_tick(&std::collections::HashSet::new()).await });
        if scenario == "revoked-during-lookup" {
            tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
                .await
                .unwrap();
            let path = format!(
                "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/bob",
                text(&route, "id")
            );
            let response = common::send(
                &app,
                owner_request(
                    "POST",
                    &path,
                    json!({"action": "remove", "revision": route["revision"]}),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            release.notify_one();
        }
        ticking.await.unwrap().unwrap();
        let stopped = s.chat_detail(text(&chat, "id")).await.unwrap();
        assert_eq!(
            stopped["run"]["chatExecution"]["messageId"], run["chatExecution"]["messageId"],
            "advanced despite {scenario}"
        );
        assert_eq!(
            stopped["run"]["status"],
            if succeeded { "succeeded" } else { "cancelled" }
        );
        if scenario.starts_with("revoked-") || scenario == "authority-unavailable" {
            assert_ne!(
                stopped["paused"], true,
                "{scenario} permanently paused the queue"
            );
            let bob = stopped["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["text"] == "Request from bob")
                .unwrap();
            assert_eq!(
                bob["status"],
                if scenario.starts_with("revoked-") {
                    "cancelled"
                } else {
                    "queued"
                }
            );
            s.chat_tick(&std::collections::HashSet::new())
                .await
                .unwrap();
            let next = s.chat_detail(text(&chat, "id")).await.unwrap();
            assert_eq!(
                next["run"]["chatExecution"]["text"],
                if scenario.starts_with("revoked-") {
                    "Request from carol"
                } else {
                    "Request from bob"
                }
            );
        } else if scenario == "unsettled-workspace" {
            assert_ne!(
                stopped["paused"], true,
                "waiting for settlement turned into a permanent pause"
            );
            let other = s
                .chat_create(json!({ "agentId": agent["id"] }))
                .await
                .unwrap();
            s.chat_send(
                text(&other, "id"),
                json!({ "id": cairn_installation::config::id(), "text": "Independent work" }),
            )
            .await
            .unwrap();
            s.chat_tick(&std::collections::HashSet::new())
                .await
                .unwrap();
            assert_eq!(
                s.chat_detail(text(&other, "id")).await.unwrap()["run"]["status"],
                "queued"
            );
            s.store
                .patch_run(run_id, json!({ "recoveryPending": false }))
                .await
                .unwrap();
            s.chat_tick(&std::collections::HashSet::new())
                .await
                .unwrap();
            assert_eq!(
                s.chat_detail(text(&chat, "id")).await.unwrap()["run"]["chatExecution"]["text"],
                "Request from bob"
            );
        } else {
            assert_eq!(stopped["paused"], true);
            if scenario == "owner-pause-before-enqueue" {
                let resumed = common::send(
                    &app,
                    owner_request(
                        "POST",
                        &format!("/api/chats/{}/pause", text(&chat, "id")),
                        json!({ "paused": false }),
                    ),
                )
                .await;
                assert_eq!(resumed.status(), StatusCode::OK);
                s.chat_tick(&std::collections::HashSet::new())
                    .await
                    .unwrap();
                assert_eq!(
                    s.chat_detail(text(&chat, "id")).await.unwrap()["run"]["chatExecution"]["text"],
                    "Request from bob",
                    "owner resume replayed the abandoned prompt instead of advancing the queued request"
                );
            }
        }
        if succeeded {
            let delivery = s
                .store
                .list("plugin-deliveries")
                .await
                .unwrap()
                .into_iter()
                .find(|delivery| delivery["id"] == run["chatExecution"]["messageId"])
                .unwrap();
            let reply = cairn_installation::plugins::work::reply(&s, &connection, &delivery)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                reply.status, "completed",
                "a fresh request cancelled the completed reply"
            );
            assert_eq!(reply.text, "Alice's answer");
        }
        if let Some(server) = authority_server {
            server.abort();
        }
        session.stop().await;
    }
}

#[tokio::test]
async fn abandoned_deliveries_never_collect_the_next_requesters_answer_or_files() {
    for (acknowledged, failed) in [(true, false), (false, false), (true, true)] {
        let (replies, mut received) = tokio::sync::mpsc::unbounded_channel();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let fenced = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (attempts, mut fences) = tokio::sync::mpsc::unbounded_channel();
        let platform = axum::Router::new()
            .route(
                "/runs/{attempt}",
                axum::routing::delete({
                    let fenced = fenced.clone();
                    move |axum::extract::Path(attempt): axum::extract::Path<String>| {
                        attempts.send(attempt).unwrap();
                        let stopped = fenced.load(std::sync::atomic::Ordering::SeqCst);
                        async move {
                            if stopped {
                                StatusCode::NO_CONTENT
                            } else {
                                StatusCode::SERVICE_UNAVAILABLE
                            }
                        }
                    }
                }),
            )
            .route(
                "/stop-barrier",
                axum::routing::get({
                    let (entered, release) = (entered.clone(), release.clone());
                    move || {
                        let (entered, release) = (entered.clone(), release.clone());
                        async move {
                            entered.notify_one();
                            release.notified().await;
                            axum::Json(json!({ "ok": true }))
                        }
                    }
                }),
            )
            .route(
                "/reply-result",
                axum::routing::post(move |axum::Json(reply): axum::Json<Value>| {
                    replies.send(reply).unwrap();
                    async { axum::Json(json!({ "ok": true })) }
                }),
            );
        let (origin, server) = common::serve_locally(platform).await;
        let (_root, s, instance, resource, app) =
            configured_fixture_with_runner(None, &origin).await;
        s.plugins.route_http_to(Some(origin));
        let agent = s
            .agent(
                json!({
                    "name": "Scoped",
                    "provider": "codex",
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
        let request = |person: &str| {
            json!({
                "id": format!("request-{person}"),
                "resource": resource,
                "author": { "id": person },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "text": format!("Request from {person}"), "newTurn": true, "trace": true },
            })
        };
        assert_eq!(
            deliver(&app, &instance, request("alice")).await.1["status"],
            "accepted"
        );
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        s.chat_tick(&std::collections::HashSet::new())
            .await
            .unwrap();
        let chat = s.chat_list().await.unwrap()[0].clone();
        let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
        let run_id = text(&detail["run"], "id").to_owned();
        let old_delivery = detail["run"]["chatExecution"]["messageId"].clone();
        // Prepared-checkpoint fixture: no native agent or VM is launched here.
        s.store
            .patch_run(
                &run_id,
                json!({
                    "status": "running",
                    "isolated": true,
                    "retry": {
                        "attempt": 3,
                        "limit": 3,
                        "nextAttemptAt": null,
                        "cause": { "kind": "server", "message": "Previous transient failures" },
                    },
                }),
            )
            .await
            .unwrap();
        s.store
            .set(
                &format!("run-checkpoint:{run_id}"),
                json!({
                    "completed": false,
                    "launched": true,
                    "isolated": true,
                    "runnerId": "old-attempt",
                }),
                None,
            )
            .await
            .unwrap();
        if acknowledged {
            s.chat_acknowledge(&run_id, old_delivery.as_str().unwrap())
                .await
                .unwrap();
        }
        assert_eq!(
            deliver(&app, &instance, request("bob")).await.1["status"],
            "accepted"
        );
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        assert!(received.try_recv().is_err());
        assert_eq!(
            deliver(
                &app,
                &instance,
                json!({
                    "id": "stop-alice",
                    "kind": "interaction",
                    "resource": resource,
                    "author": { "id": "alice" },
                    "context": { "channel": "C1", "thread": "thread" },
                    "body": { "chat": chat["id"], "delivery": old_delivery, "stopDelay": if failed { json!("wait") } else { json!(true) } },
                })
            )
            .await
            .1["status"],
            "accepted"
        );
        let pump = tokio::spawn({
            let (s, connection) = (s.clone(), connection.clone());
            async move {
                let result = runtime::pump(&s, &connection, &mut session).await;
                (session, result)
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        if failed {
            // Native provider output after its last allowed automatic retry.
            s.store
                .event(&run_id, "turn.failed", "Previous transient failures", None)
                .await
                .unwrap();
            s.store
                .patch_run(&run_id, json!({ "status": "failed" }))
                .await
                .unwrap();
        }
        // The event handler is still awaiting platform I/O, so the reply pump
        // misses the predecessor's terminal state entirely.
        s.chat_tick(&std::collections::HashSet::new())
            .await
            .unwrap();
        let next = s.chat_detail(text(&chat, "id")).await.unwrap();
        assert_eq!(next["run"]["chatExecution"]["text"], "Request from bob");
        assert!(
            next["run"]["retry"].is_null(),
            "the independent request inherited its predecessor's spent retry budget"
        );
        let new_delivery = next["run"]["chatExecution"]["messageId"].clone();
        s.accounts
            .create(
                &s,
                cairn_installation::provider::Provider::Codex,
                "Not connected fixture",
            )
            .await
            .unwrap();
        // Drive the real native scheduler through its controller fencing boundary.
        // The prior process did not emit turn.completed; that is not stop evidence.
        s.worker.tick(&s).await.unwrap();
        assert_eq!(fences.try_recv().unwrap(), "old-attempt");
        let waiting = s.chat_detail(text(&chat, "id")).await.unwrap();
        assert_eq!(waiting["run"]["status"], "queued");
        assert_eq!(waiting["run"]["recoveryPending"], true);
        assert!(s.worker.active.lock().await.is_empty());
        fenced.store(true, std::sync::atomic::Ordering::SeqCst);
        s.worker.tick(&s).await.unwrap();
        assert_eq!(fences.try_recv().unwrap(), "old-attempt");
        let released = s.chat_detail(text(&chat, "id")).await.unwrap();
        assert_eq!(released["run"]["recoveryPending"], false);
        assert_eq!(released["run"]["chatExecution"]["messageId"], new_delivery);
        assert!(s.worker.active.lock().await.is_empty());
        // This fixture has no connected engine account; after fencing it waits for one.
        // Supply native agent output at the existing event seam below.
        s.store
            .patch_run(&run_id, json!({ "status": "running" }))
            .await
            .unwrap();
        s.chat_acknowledge(&run_id, new_delivery.as_str().unwrap())
            .await
            .unwrap();
        s.store
            .event(
                &run_id,
                "item.completed",
                "",
                Some(json!({
                    "item": { "type": "agent_message", "text": "Bob's answer" },
                })),
            )
            .await
            .unwrap();
        s.store
            .event(
                &run_id,
                "artifact",
                "",
                Some(json!({
                    "id": "bob-artifact",
                    "name": "bob.txt",
                    "title": "Bob's result",
                    "mediaType": "text/plain",
                    "size": 3,
                })),
            )
            .await
            .unwrap();
        s.store
            .event(&run_id, "turn.completed", "", None)
            .await
            .unwrap();
        s.store
            .patch_run(&run_id, json!({ "status": "succeeded" }))
            .await
            .unwrap();
        release.notify_one();
        let (session, result) = pump.await.unwrap();
        result.unwrap();
        let mut replies = Vec::new();
        for _ in 0..2 {
            replies.push(
                tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let alice = replies
            .iter()
            .find(|reply| reply["userId"] == "alice")
            .unwrap();
        assert_eq!(
            alice["status"],
            if failed { "failed" } else { "cancelled" },
            "the abandoned delivery consumed a later completion"
        );
        assert!(!text(alice, "text").contains("Bob"));
        assert_eq!(alice["files"], json!([]));
        let bob = replies
            .iter()
            .find(|reply| reply["userId"] == "bob")
            .unwrap();
        assert_eq!(bob["status"], "completed");
        assert_eq!(bob["text"], "Bob's answer");
        assert_eq!(bob["files"][0]["id"], "bob-artifact");
        session.stop().await;
        server.abort();
    }
}

#[tokio::test]
async fn a_sending_ordinary_request_keeps_its_place_before_an_independent_request() {
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
    let request = |person: &str| {
        json!({
            "id": format!("request-{person}"),
            "resource": resource,
            "author": { "id": person },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "text": format!("Request from {person}"), "newTurn": true },
        })
    };
    assert_eq!(
        deliver(&app, &instance, request("alice")).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.chat_list().await.unwrap()[0].clone();
    let first = s.chat_detail(text(&chat, "id")).await.unwrap();
    let run_id = text(&first["run"], "id");
    s.chat_acknowledge(run_id, text(&first["run"]["chatExecution"], "messageId"))
        .await
        .unwrap();
    s.store
        .event(run_id, "turn.completed", "", None)
        .await
        .unwrap();
    s.store
        .patch_run(run_id, json!({ "status": "succeeded" }))
        .await
        .unwrap();
    s.store
        .set(
            &format!("run-checkpoint:{run_id}"),
            json!({ "launched": false }),
            None,
        )
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();

    // The native owner can also use this shared Cairn conversation. Its ordinary
    // steering request may already be on the way to the native process at Stop.
    let messages_path = format!("/api/chats/{}/messages", text(&chat, "id"));
    let owner_message = cairn_installation::config::id();
    let sent = common::send(
        &app,
        owner_request(
            "POST",
            &messages_path,
            json!({
                "id": owner_message,
                "text": "Owner's original request",
            }),
        ),
    )
    .await;
    assert_eq!(sent.status(), StatusCode::OK);
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    assert_eq!(
        s.chat_detail(text(&chat, "id")).await.unwrap()["run"]["chatExecution"]["messageId"],
        owner_message
    );
    // Prepared, before-launch checkpoint; no native process or VM is started.
    s.store
        .patch_run(
            run_id,
            json!({ "status": "running", "isolated": true, "recoveryPending": false }),
        )
        .await
        .unwrap();
    s.store
        .set(
            &format!("run-checkpoint:{run_id}"),
            json!({ "launched": false }),
            None,
        )
        .await
        .unwrap();
    let steering = cairn_installation::config::id();
    let sent = common::send(
        &app,
        owner_request(
            "POST",
            &messages_path,
            json!({
                "id": steering,
                "text": "Owner's earlier steering request",
                "mode": "steer",
            }),
        ),
    )
    .await;
    assert_eq!(sent.status(), StatusCode::OK);
    s.chat_tick(&std::collections::HashSet::from([run_id.to_owned()]))
        .await
        .unwrap();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    assert!(
        detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| { message["id"] == steering && message["status"] == "sending" })
    );
    assert_eq!(
        deliver(&app, &instance, request("bob")).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let stopped = common::send(
        &app,
        owner_request(
            "POST",
            &format!("/api/chats/{}/stop", text(&chat, "id")),
            json!({}),
        ),
    )
    .await;
    assert_eq!(stopped.status(), StatusCode::OK);
    assert_eq!(
        s.chat_detail(text(&chat, "id")).await.unwrap()["run"]["status"],
        "cancelled"
    );
    let resumed = common::send(
        &app,
        owner_request(
            "POST",
            &format!("/api/chats/{}/pause", text(&chat, "id")),
            json!({ "paused": false }),
        ),
    )
    .await;
    assert_eq!(resumed.status(), StatusCode::OK);
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let resumed = s.chat_detail(text(&chat, "id")).await.unwrap();
    assert_eq!(
        resumed["run"]["chatExecution"]["messageId"], owner_message,
        "the independent request bypassed an ordinary message already being sent"
    );
    session.stop().await;
}

#[tokio::test]
async fn an_authorized_participant_can_reset_a_settled_thread_without_erasing_its_history() {
    let (_root, s, instance, resource, app) = fixture().await;
    let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
    let fail_first_reset = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let upstream = axum::Router::new().route(
        "/reset-result",
        axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
            let fail = value["reset"] == true
                && fail_first_reset.swap(false, std::sync::atomic::Ordering::SeqCst);
            results.send(value).unwrap();
            async move {
                (
                    if fail {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    },
                    axum::Json(json!({ "ok": true })),
                )
            }
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
            "name": "Thread", "enabled": true, "match": { "channel": "C1" },
            "agentId": agent["id"], "userIds": ["alice", "bob"],
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
    let message = |id: &str, author: &str| {
        json!({
            "id": id, "resource": resource, "author": { "id": author },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "text": format!("Request from {author}") },
        })
    };
    assert_eq!(
        deliver(&app, &instance, message("first", "alice")).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let original = s.chat_list().await.unwrap()[0].clone();
    let chat = text(&original, "id");
    let reset = |id: &str| {
        json!({
            "id": id, "kind": "interaction", "resource": resource, "author": { "id": "bob" },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "reset": true, "chat": chat },
        })
    };
    assert_eq!(
        deliver(&app, &instance, reset("before-settlement")).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        received.recv().await.unwrap()["status"],
        "refused",
        "reset orphaned queued work"
    );

    // Native lifecycle fixture: no process or VM is launched.
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = s.chat_detail(chat).await.unwrap();
    let run = text(&detail["run"], "id");
    s.chat_acknowledge(run, text(&detail["run"]["chatExecution"], "messageId"))
        .await
        .unwrap();
    s.store
        .patch_run(
            run,
            json!({ "status": "running", "recoveryPending": false }),
        )
        .await
        .unwrap();
    assert_eq!(
        deliver(&app, &instance, reset("active-reset")).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        received.recv().await.unwrap()["status"],
        "refused",
        "reset orphaned active work"
    );
    // Register waits while their source delivery is still active. Scheduling
    // after the reply settles would create a continuation with no destination.
    let configuration = s
        .mcps
        .run_configuration(&s, &s.store.run(run).await.unwrap())
        .await
        .unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    for mode in ["timed", "due", "condition", "paused-condition"] {
        s.store
            .patch_run(run, json!({ "status": "running" }))
            .await
            .unwrap();
        cairn_installation::chats::sleep::schedule(
            &s,
            bearer,
            &json!({
                "seconds": if mode == "due" { 1 } else { 3600 }, "reason": "Await provider result",
            }),
        )
        .await
        .unwrap();
        if mode.contains("condition") {
            // Prepared native wait state. Source agents cannot acquire installation
            // access to register run_finished; this also represents a saved wait
            // whose earlier grants have since been reduced, awaiting its native result.
            let mut waiting = s.get("chats", chat).await.unwrap();
            waiting["wakeup"]["condition"] = json!({
                "type": "run_finished", "runId": cairn_installation::config::id(),
            });
            s.store.put("chats", waiting).await.unwrap();
        }
        s.store
            .patch_run(run, json!({ "status": "succeeded" }))
            .await
            .unwrap();
        if mode == "due" {
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        }
        if mode.starts_with("paused") {
            s.chat_pause(chat, true).await.unwrap();
        }
        assert_eq!(
            deliver(&app, &instance, reset(&format!("waiting-{mode}")))
                .await
                .1["status"],
            "accepted"
        );
        runtime::pump(&s, &connection, &mut session).await.unwrap();
        assert_eq!(
            received.recv().await.unwrap()["status"],
            "refused",
            "reset orphaned {mode} continuation"
        );
        assert!(s.chat_detail(chat).await.unwrap()["wakeup"].is_object());
        // The owner explicitly cancels this future work; reset itself must not.
        s.chat_pause(chat, false).await.unwrap();
        s.worker.cancel(&s, run).await.unwrap();
        assert!(s.chat_detail(chat).await.unwrap()["wakeup"].is_null());
    }

    s.store
        .event(run, "turn.completed", "", None)
        .await
        .unwrap();
    s.store
        .patch_run(
            run,
            json!({ "status": "succeeded", "recoveryPending": false }),
        )
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();

    s.store
        .patch_run(run, json!({ "status": "running" }))
        .await
        .unwrap();
    let late_wait = cairn_installation::chats::sleep::schedule(
        &s,
        bearer,
        &json!({ "seconds": 3600, "reason": "After the reply already settled" }),
    )
    .await
    .unwrap_err();
    assert_eq!(late_wait.status, 409);
    assert!(s.chat_detail(chat).await.unwrap()["wakeup"].is_null());
    s.store
        .patch_run(run, json!({ "status": "succeeded" }))
        .await
        .unwrap();

    s.store
        .patch_run(run, json!({ "recoveryPending": true }))
        .await
        .unwrap();
    assert_eq!(
        deliver(&app, &instance, reset("before-recovery")).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        received.recv().await.unwrap()["status"],
        "refused",
        "reset orphaned recovery"
    );
    s.store
        .patch_run(run, json!({ "recoveryPending": false }))
        .await
        .unwrap();
    let mut wrong_thread = reset("another-thread");
    wrong_thread["context"]["thread"] = json!("unrelated-thread");
    assert_eq!(
        deliver(&app, &instance, wrong_thread).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        received.recv().await.unwrap()["status"],
        "refused",
        "another thread could reset this one"
    );

    assert_eq!(
        deliver(&app, &instance, reset("settled-reset")).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let result = received.recv().await.unwrap();
    assert_eq!(
        result["reset"], true,
        "settled thread could not be reset: {result}"
    );
    assert_eq!(result["conversation"], chat);
    assert_eq!(
        s.chat_list().await.unwrap().len(),
        1,
        "reset created an empty conversation"
    );
    assert_eq!(
        s.chat_detail(chat).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        deliver(&app, &instance, message("after-reset", "bob"))
            .await
            .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let chats = s.chat_list().await.unwrap();
    assert_eq!(
        chats.len(),
        2,
        "the next message reused the retired thread's history"
    );
    let next = chats.iter().find(|value| value["id"] != chat).unwrap();
    assert_ne!(next["id"], chat);
    assert_eq!(
        s.chat_detail(text(next, "id")).await.unwrap()["messages"][0]["text"],
        "Request from bob"
    );
    assert_eq!(
        s.chat_detail(chat).await.unwrap()["messages"][0]["text"],
        "Request from alice"
    );
    // The first reset committed, but its HTTP acknowledgement failed. Its durable
    // handler retry must return that receipt after Bob has created the new binding.
    tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        received.recv().await.unwrap(),
        result,
        "reset replay lost its committed receipt"
    );
    assert_eq!(
        deliver(&app, &instance, message("same-new-thread", "bob"))
            .await
            .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        s.chat_list().await.unwrap().len(),
        2,
        "reset retry detached the replacement conversation"
    );
    assert_eq!(
        deliver(&app, &instance, reset("stale-reset-button"))
            .await
            .1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(received.recv().await.unwrap()["status"], "refused");
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn route_eligibility_uses_native_resource_grants_without_disclosing_owner_data() {
    let (_root, s, instance, resource, app) = fixture().await;
    let safe = s.agent(json!({
        "name": "Dedicated read-only",
        "access": {"sandbox": "read-only", "projects": [], "installation": false, "github": false, "mcps": []},
    }), None).await.unwrap();
    let unsafe_agent = s
        .agent(
            json!({
                "name": "Installation agent",
                "access": {"projects": [], "installation": true, "github": false, "mcps": []},
            }),
            None,
        )
        .await
        .unwrap();
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/route-eligibility?accessMode=everyone"
    );
    let read = || owner_request("GET", &path, Value::Null);
    let response = common::send(&app, read()).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let result = common::read_json(response).await;
    let items = result["items"].as_array().unwrap();
    let current = items
        .iter()
        .find(|item| item["agentId"] == safe["id"])
        .unwrap();
    assert_eq!(current["eligible"], true);
    assert!(current.get("grantedAccess").is_none());
    let blocked = items
        .iter()
        .find(|item| item["agentId"] == unsafe_agent["id"])
        .unwrap();
    assert_eq!(blocked["eligible"], false);
    assert!(!text(blocked, "reason").is_empty());

    let grants =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/grants");
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &grants,
            json!({
                "agentId": safe["id"], "operations": ["read"],
            }),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let grant = common::read_json(response).await;
    let response = common::send(&app, read()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let result = common::read_json(response).await;
    let current = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["agentId"] == safe["id"])
        .unwrap();
    assert_eq!(
        current["eligible"], false,
        "resource-granted agent must not accept everyone"
    );
    assert!(!text(current, "reason").is_empty());

    let route_path =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes");
    let response = common::send(&app, owner_request("POST", &route_path, json!({
        "name": "Public", "enabled": true, "match": {}, "agentId": safe["id"], "accessMode": "everyone",
    }))).await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "preview must match save"
    );

    let response = common::send(
        &app,
        owner_request(
            "DELETE",
            &format!("{grants}/{}", text(&grant, "id")),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = common::send(&app, read()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let result = common::read_json(response).await;
    let current = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["agentId"] == safe["id"])
        .unwrap();
    assert_eq!(current["eligible"], true);

    let mut member = read();
    member
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Member,
            "fixture-member",
        ));
    let response = common::send(&app, member).await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "eligibility and owner resource metadata stay owner-only"
    );
}

#[tokio::test]
async fn stale_route_edits_cannot_restore_a_revoked_participant() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s.agent(json!({
        "name": "Dedicated", "access": {"projects": [], "installation": false, "github": false, "mcps": []},
    }), None).await.unwrap();
    let mut original = json!({
        "name": "Support", "enabled": true, "match": {"channel": "C1"},
        "agentId": agent["id"], "accessMode": "allowlist", "userIds": ["alice", "bob"],
    });
    let route = create_route(&app, &instance, &resource, original.clone()).await;
    original["revision"] = route["revision"].clone();
    for person in ["alice", "bob"] {
        assert_eq!(
            deliver(
                &app,
                &instance,
                json!({
                    "id": person, "resource": resource, "author": {"id": person},
                    "context": {"channel": "C1", "thread": person}, "body": {"text": person},
                })
            )
            .await
            .1["status"],
            "accepted"
        );
    }
    let route_path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}",
        text(&route, "id")
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &format!("{route_path}/people/bob"),
            json!({"action": "remove", "revision": route["revision"]}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated = common::read_json(response).await;
    assert_eq!(updated["userIds"], json!(["alice"]));
    assert_ne!(updated["revision"], route["revision"]);

    let response = common::send(
        &app,
        owner_request(
            "POST",
            &format!("{route_path}/people/alice"),
            json!({"action": "remove", "revision": route["revision"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "stale people controls must refresh too"
    );

    let response = common::send(&app, owner_request("PUT", &route_path, original)).await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "an editor opened before revocation must refresh, not restore the revoked participant"
    );
    let response = common::send(
        &app,
        owner_request(
            "GET",
            &format!(
                "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes"
            ),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let routes = common::read_json(response).await;
    let current = routes["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|current| current["id"] == route["id"])
        .unwrap();
    assert_eq!(current["userIds"], json!(["alice"]));
    assert_eq!(current["revision"], updated["revision"]);

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
    let chats = s.chat_list().await.unwrap();
    assert_eq!(
        chats.len(),
        1,
        "removing Bob must preserve Alice's accepted work, while revoking Bob's"
    );
    assert_eq!(
        s.chat_detail(text(&chats[0], "id")).await.unwrap()["messages"][0]["text"],
        "alice"
    );
    session.stop().await;

    let response = common::send(
        &app,
        owner_request(
            "DELETE",
            &route_path,
            json!({"revision": route["revision"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a stale delete must not remove a changed route"
    );
    let response = common::send(
        &app,
        owner_request(
            "DELETE",
            &route_path,
            json!({"revision": updated["revision"]}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn external_routes_require_an_agent_that_can_run_without_native_host_provenance() {
    let (_root, s, instance, resource, app) = fixture().await;
    let node = json!({
        "id": cairn_installation::config::id(),
        "name": "Trusted native fixture",
        "nativeEnabled": true,
        "nativeAccounts": ["fixture-owner"],
        "capabilities": { "backend": "native" },
    });
    s.store.put("nodes", node.clone()).await.unwrap();
    let agent = s
        .agent(
            json!({
                "name": "Native external candidate",
                "access": {
                    "backend": "native",
                    "nodes": [node["id"]],
                    "projects": [],
                    "installation": false,
                    "github": false,
                    "mcps": [],
                    "sandbox": "read-only",
                },
            }),
            None,
        )
        .await
        .unwrap();
    let base = format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}");
    let response = common::send(
        &app,
        owner_request(
            "GET",
            &format!("{base}/route-eligibility?accessMode=requests"),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result = common::read_json(response).await;
    let item = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["agentId"] == agent["id"])
        .unwrap();
    assert_eq!(
        item["eligible"], false,
        "external routes have no native host origin"
    );
    assert!(text(item, "reason").contains("native node"));
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &format!("{base}/routes"),
            json!({
                "name": "Native route",
                "enabled": true,
                "agentId": agent["id"],
                "match": {},
                "accessMode": "requests",
            }),
        ),
    )
    .await;
    let status = response.status();
    let refused = common::read_json(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert!(refused.to_string().contains("native node"));
    let response = common::send(
        &app,
        owner_request("GET", &format!("{base}/routes"), Value::Null),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(common::read_json(response).await["items"], json!([]));
}

#[tokio::test]
async fn route_eligibility_matches_the_exact_project_and_allows_paused_configuration() {
    let (root, s, instance, resource, app) = fixture().await;
    let project = s
        .project(
            json!({ "name": "Support", "path": root.path().join("home") }),
            None,
        )
        .await
        .unwrap();
    let agent = s
        .agent(
            json!({
                "name": "Project helper",
                "access": {
                    "projects": [project["id"]],
                    "installation": false,
                    "github": false,
                    "mcps": [],
                    "sandbox": "read-only",
                },
            }),
            None,
        )
        .await
        .unwrap();
    let base = format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}");
    for (query, eligible) in [
        ("accessMode=everyone".to_owned(), false),
        (
            format!("accessMode=everyone&projectId={}", text(&project, "id")),
            true,
        ),
        (
            format!("accessMode=requests&projectId={}", text(&project, "id")),
            true,
        ),
    ] {
        let response = common::send(
            &app,
            owner_request(
                "GET",
                &format!("{base}/route-eligibility?{query}"),
                Value::Null,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let result = common::read_json(response).await;
        let item = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["agentId"] == agent["id"])
            .unwrap();
        assert_eq!(item["eligible"], eligible, "{query}");
        for item in result["items"].as_array().unwrap() {
            assert!(
                item.as_object()
                    .unwrap()
                    .keys()
                    .all(|key| matches!(key.as_str(), "agentId" | "eligible" | "reason"))
            );
        }
    }
    for query in [
        "accessMode=invalid".to_owned(),
        "projectId=bad".to_owned(),
        format!("projectId={}", uuid::Uuid::new_v4()),
    ] {
        let response = common::send(
            &app,
            owner_request(
                "GET",
                &format!("{base}/route-eligibility?{query}"),
                Value::Null,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }

    connections::set_enabled(&s, "router", &instance, &json!({"enabled": false}))
        .await
        .unwrap();
    let saved = resources::get(&s, "router", &instance, "workspace", &resource)
        .await
        .unwrap();
    resources::save(
        &s,
        "router",
        &instance,
        "workspace",
        &json!({
            "name": saved.name,
            "enabled": false,
            "settings": saved.settings,
            "revision": saved.revision,
        }),
        Some(&resource),
    )
    .await
    .unwrap();
    let response = common::send(
        &app,
        owner_request(
            "GET",
            &format!(
                "{base}/route-eligibility?accessMode=everyone&projectId={}",
                text(&project, "id")
            ),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "paused resources still allow owner configuration"
    );
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Prepared route",
            "enabled": false,
            "agentId": agent["id"],
            "projectId": project["id"],
            "accessMode": "everyone",
        }),
    )
    .await;
}

#[tokio::test]
async fn reapproving_a_participant_never_revives_their_old_admitted_work() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Dedicated",
                "access": {
                    "projects": [],
                    "installation": false,
                    "github": false,
                    "mcps": [],
                },
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
            "name": "Requests",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "accessMode": "requests",
            "userIds": ["alice", "bob"],
        }),
    )
    .await;
    let event = |id: &str, person: &str| {
        json!({
            "id": id,
            "resource": resource,
            "author": { "id": person },
            "context": { "channel": "C1", "thread": id },
            "body": { "text": id },
        })
    };
    for (id, person) in [("old-bob", "bob"), ("alice", "alice")] {
        assert_eq!(
            deliver(&app, &instance, event(id, person)).await.1["status"],
            "accepted"
        );
    }
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/bob",
        text(&route, "id")
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &path,
            json!({
                "action": "remove",
                "revision": route["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let removed = common::read_json(response).await;
    assert_eq!(
        deliver(&app, &instance, event("new-access-request", "bob"))
            .await
            .1["status"],
        "requested"
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &path,
            json!({
                "action": "approve",
                "revision": removed["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        deliver(&app, &instance, event("new-bob", "bob")).await.1["status"],
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
    let mut messages = Vec::new();
    for chat in s.chat_list().await.unwrap() {
        let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
        messages.push(text(&detail["messages"][0], "text").to_owned());
    }
    messages.sort();
    assert_eq!(
        messages,
        vec!["alice", "new-bob"],
        "reapproval revived Bob's old admission or revoked unrelated Alice"
    );
    session.stop().await;
}

#[tokio::test]
async fn reapproving_a_person_cannot_release_their_old_http_result() {
    let (_root, s, instance, resource, app) = fixture().await;
    let original = registry::active(&s, "router").await.unwrap();
    let mut manifest = serde_json::to_value(&original.manifest).unwrap();
    manifest["version"] = json!("2");
    let package = registry::install(&s, &json!({
        "manifest": manifest,
        "source": r#"return {
            webhook = function(event)
                local result = cairn.inbound.deliver(cairn.json.decode(event.body))
                return {status = 200, body = cairn.json.encode(result)}
            end,
            message = function(event)
                local response = cairn.http.request({url = 'https://api.example.test/held-result'})
                return cairn.conversations.submit({text = response.text, thread = event.context.thread, shared = true})
            end,
        }"#,
    })).await.unwrap();
    registry::approve(&s, "router", "2", text(&package, "digest"))
        .await
        .unwrap();
    let agent = s
        .agent(
            json!({
                "name": "Dedicated",
                "access": {
                    "projects": [],
                    "installation": false,
                    "github": false,
                    "mcps": [],
                },
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
            "name": "Requests",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "accessMode": "requests",
            "userIds": ["bob"],
        }),
    )
    .await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = axum::Router::new().route(
        "/held-result",
        axum::routing::get({
            let (entered, release) = (entered.clone(), release.clone());
            move || {
                let (entered, release) = (entered.clone(), release.clone());
                async move {
                    entered.notify_one();
                    release.notified().await;
                    "private-result-from-retired-grant"
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    s.plugins
        .route_http_to(Some(format!("http://{}", listener.local_addr().unwrap())));
    let server = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let event = |id: &str| {
        json!({
            "id": id,
            "resource": resource,
            "author": { "id": "bob" },
            "context": { "channel": "C1", "thread": "thread" },
            "body": { "text": "Read private data" },
        })
    };
    // Prove the handler can export this exact response when authority is intact.
    // Otherwise an unrelated script error could make the revocation check pass.
    let mut control = event("control");
    control["context"]["thread"] = json!("control");
    assert_eq!(
        deliver(&app, &instance, control).await.1["status"],
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
    release.notify_one();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    entered.notified().await;
    let control_chats = s.chat_list().await.unwrap();
    assert_eq!(control_chats.len(), 1);
    let control_chat = text(&control_chats[0], "id").to_owned();
    assert_eq!(
        s.chat_detail(&control_chat).await.unwrap()["messages"][0]["text"],
        "private-result-from-retired-grant"
    );
    session.stop().await;
    assert_eq!(
        deliver(&app, &instance, event("old-bob")).await.1["status"],
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
    let worker = {
        let s = s.clone();
        tokio::spawn(async move {
            let result = runtime::pump(&s, &connection, &mut session).await;
            session.stop().await;
            result
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/bob",
        text(&route, "id")
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &path,
            json!({
                "action": "remove",
                "revision": route["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let removed = common::read_json(response).await;
    assert_eq!(
        deliver(&app, &instance, event("new-access-request"))
            .await
            .1["status"],
        "requested"
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &path,
            json!({
                "action": "approve",
                "revision": removed["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.abort();
    let chats = s.chat_list().await.unwrap();
    assert_eq!(
        chats.len(),
        1,
        "the old provider result was exported after reapproval"
    );
    assert_eq!(chats[0]["id"], control_chat);
}

#[tokio::test]
async fn unversioned_routed_admissions_do_not_acquire_a_persons_current_grant() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Dedicated",
                "access": {
                    "projects": [],
                    "installation": false,
                    "github": false,
                    "mcps": [],
                },
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
            "name": "Original grant",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "userIds": ["alice"],
        }),
    )
    .await;
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "unversioned",
                "resource": resource,
                "author": { "id": "alice" },
                "context": { "channel": "C1", "thread": "thread" },
                "body": { "text": "A message written by an earlier draft binary" },
            })
        )
        .await
        .1["status"],
        "accepted"
    );
    let events = s.store.list("plugin-events").await.unwrap();
    assert_eq!(events.len(), 1);
    let mut legacy = events[0].clone();
    legacy["routing"].as_object_mut().unwrap().remove("person");
    s.store.put("plugin-events", legacy).await.unwrap();
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
        "an old unversioned admission silently acquired current authority"
    );
    session.stop().await;
}

async fn root_scope_call(
    s: &Service,
    bearer: &str,
    command: bool,
) -> cairn_installation::error::Result<Value> {
    if command {
        s.plugins
            .command(
                s,
                bearer,
                &json!({ "plugin": "rootreader", "command": "read" }),
            )
            .await
            .map(|value| value["value"].clone())
    } else {
        agents::call_tool(s, bearer, "rootreader_read", &json!({}))
            .await
            .map(|value| value.expect("the explicitly granted tool is available"))
    }
}

async fn root_scopes_keep_the_source_participants_authority(command: bool) {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Dedicated",
                "access": { "projects": [], "installation": false, "github": false, "mcps": [] },
            }),
            None,
        )
        .await
        .unwrap();
    let package = registry::install(
        &s,
        &json!({
            "manifest": {
                "id": "rootreader", "name": "Root reader", "version": "1", "apiVersion": 2,
                "capabilities": ["http"], "hosts": ["api.example.test"], "agentAccess": {},
                "commands": [{ "name": "read", "output": "json" }],
                "tools": [{
                    "name": "rootreader_read", "description": "Read through the granted instance",
                    "inputSchema": { "type": "object", "additionalProperties": false },
                }],
            },
            "source": r#"local function read(event)
            return cairn.http.request({url = 'https://api.example.test/root-result',
                instance = event.instances[1].id}).json
        end
        return {command = read, tool = read}"#,
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "rootreader", "1", text(&package, "digest"))
        .await
        .unwrap();
    connections::save(
        &s,
        "rootreader",
        &json!({
            "name": "Explicit target grant", "enabled": true, "agentIds": [agent["id"]],
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
            "name": "Restricted source", "enabled": true, "match": { "channel": "C1" },
            "agentId": agent["id"], "accessMode": "requests", "userIds": ["bob"],
        }),
    )
    .await;
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = axum::Router::new().route(
        "/root-result",
        axum::routing::get({
            let (hold, entered, release) = (hold.clone(), entered.clone(), release.clone());
            move || {
                let (hold, entered, release) = (hold.clone(), entered.clone(), release.clone());
                async move {
                    if hold.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({ "value": "root-granted-private-data" }))
                }
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "old-bob", "resource": resource, "author": { "id": "bob" },
                "context": { "channel": "C1", "thread": "old-bob" },
                "body": { "text": "Read through my restricted route" },
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
    session.stop().await;
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.chat_list().await.unwrap()[0].clone();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    // Native lifecycle fixture: prepare the admitted turn without launching a VM.
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({
                "status": "running", "isolated": true,
            }),
        )
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
    assert_eq!(
        root_scope_call(&s, &bearer, command).await.unwrap()["value"],
        "root-granted-private-data",
        "positive control must reach the real plugin HTTP effect"
    );
    hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let pending = {
        let (s, bearer) = (s.clone(), bearer.clone());
        tokio::spawn(async move { root_scope_call(&s, &bearer, command).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/bob",
        text(&route, "id"),
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &path,
            json!({
                "action": "remove", "revision": route["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let removed = common::read_json(response).await;
    let removed_call = root_scope_call(&s, &bearer, command).await;
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "new-access-request", "resource": resource, "author": { "id": "bob" },
                "context": { "channel": "C1", "thread": "new-access-request" },
                "body": { "text": "Request a fresh grant" },
            })
        )
        .await
        .1["status"],
        "requested"
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &path,
            json!({
                "action": "approve", "revision": removed["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reapproved_call = root_scope_call(&s, &bearer, command).await;
    release.notify_one();
    let old_result = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    server.abort();
    assert!(
        removed_call.is_err() && reapproved_call.is_err() && old_result.is_err(),
        "root scope escaped source revocation: removed={}, reapproved={}, old_result={}",
        removed_call.is_ok(),
        reapproved_call.is_ok(),
        old_result.is_ok()
    );
}

#[tokio::test]
async fn root_commands_cannot_bypass_the_source_participants_revocation() {
    root_scopes_keep_the_source_participants_authority(true).await;
}

#[tokio::test]
async fn root_tools_cannot_bypass_the_source_participants_revocation() {
    root_scopes_keep_the_source_participants_authority(false).await;
}

async fn source_package_root_call(
    s: &Service,
    bearer: &str,
    instance: &str,
    command: bool,
) -> cairn_installation::error::Result<Value> {
    if command {
        s.plugins
            .command(
                s,
                bearer,
                &json!({
                    "plugin": "router", "command": "root-read",
                    "args": ["--instance", instance, "--channel", "C2"],
                }),
            )
            .await
            .map(|value| value["value"].clone())
    } else {
        agents::call_tool(
            s,
            bearer,
            "router_root",
            &json!({"instance": instance, "channel": "C2"}),
        )
        .await
        .map(|value| value.expect("the explicitly granted tool is available"))
    }
}

async fn a_source_package_root_grant_does_not_replace_a_route_scope(command: bool) {
    let (_root, s, instance, resource, app) = fixture().await;
    let original = registry::active(&s, "router").await.unwrap();
    let mut manifest = serde_json::to_value(&original.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["commands"] = json!([{
        "name": "root-read", "output": "json", "arguments": [
            {"name": "instance", "flag": "--instance", "required": true},
            {"name": "channel", "flag": "--channel", "required": true},
        ],
    }]);
    manifest["tools"].as_array_mut().unwrap().push(json!({
        "name": "router_root", "description": "Read through an instance-wide grant",
        "inputSchema": {"type": "object", "required": ["instance", "channel"],
            "properties": {"instance": {"type": "string"}, "channel": {"type": "string"}}},
    }));
    let installed = registry::install(&s, &json!({
        "manifest": manifest,
        "source": r#"local function root_read(event)
            local input = event.arguments or event.input
            return cairn.http.request({instance = input.instance,
                url = 'https://api.example.test/root?channel=' .. input.channel,
                auth = {secret = 'signature', header = 'authorization', prefix = 'Bearer '}}).json
        end
        return {
            command = root_read,
            tool = function(event)
                if event.tool == 'router_scope' then return event.input end
                return root_read(event)
            end,
            webhook = function(event)
                local result = cairn.inbound.deliver(cairn.json.decode(event.body))
                return {status = 200, body = cairn.json.encode(result)}
            end,
            message = function(event)
                return cairn.conversations.submit({text = event.text, thread = event.context.thread, shared = true})
            end,
        }"#,
    })).await.unwrap();
    registry::approve(&s, "router", "2", text(&installed, "digest"))
        .await
        .unwrap();
    let agent = s.agent(json!({
        "name": "Dedicated", "access": {"projects": [], "installation": false, "github": false, "mcps": []},
    }), None).await.unwrap();
    connections::save(
        &s,
        "router",
        &json!({
            "name": "Source app", "enabled": true, "agentIds": [agent["id"]],
            "settings": {"clientId": "source-client"},
        }),
        Some(&instance),
    )
    .await
    .unwrap();
    let other = connections::save(
        &s,
        "router",
        &json!({
            "name": "Other app", "enabled": true, "agentIds": [agent["id"]],
            "settings": {"clientId": "other-client"}, "secrets": {"signature": "other-app-fixture"},
        }),
        None,
    )
    .await
    .unwrap();
    let other = text(&other, "id");
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "C1 only", "enabled": true, "match": {"channel": "C1"},
            "agentId": agent["id"], "userIds": ["bob"],
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
                    "agentId": agent["id"], "profile": "read",
                })
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = axum::Router::new().route(
        "/root",
        axum::routing::get({
            let seen = seen.clone();
            move |headers: axum::http::HeaderMap| {
                let seen = seen.clone();
                async move {
                    let auth = headers["authorization"].to_str().unwrap();
                    assert!(
                        auth == format!("Bearer {SECRET}") || auth == "Bearer other-app-fixture"
                    );
                    seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(json!({"channel": "C2", "value": "outside-the-source-route"}))
                }
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    // Explicit native task authority has no routed source: the independent
    // instance grants really permit both reads, with the real injected secrets.
    let task = s
        .task(
            json!({"name": "Control", "agentId": agent["id"], "prompt": "Probe"}),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&run, "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    for target in [instance.as_str(), other] {
        assert_eq!(
            source_package_root_call(
                &s,
                text(&config["env"], "CAIRN_MCP_RUN_TOKEN"),
                target,
                command
            )
            .await
            .unwrap()["value"],
            "outside-the-source-route"
        );
    }
    s.store
        .patch_run(text(&run, "id"), json!({"status": "succeeded"}))
        .await
        .unwrap();
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(deliver(&app, &instance, json!({
        "id": "bob", "resource": resource, "author": {"id": "bob"},
        "context": {"channel": "C1", "thread": "thread"}, "body": {"text": "Work inside C1"},
    })).await.1["status"], "accepted");
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
    session.stop().await;
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.chat_list().await.unwrap()[0].clone();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&config["env"], "CAIRN_MCP_RUN_TOKEN");
    let scoped = agents::call_tool(
        &s,
        bearer,
        "router_scope",
        &json!({"resource": resource, "channel": "C1"}),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(scoped["channel"], "C1");
    assert!(
        agents::call_tool(
            &s,
            bearer,
            "router_scope",
            &json!({"resource": resource, "channel": "C2"})
        )
        .await
        .is_err()
    );
    let same_instance = source_package_root_call(&s, bearer, &instance, command).await;
    let other_instance = source_package_root_call(&s, bearer, other, command).await;
    server.abort();
    assert!(
        same_instance.is_err()
            && other_instance.is_err()
            && seen.load(std::sync::atomic::Ordering::SeqCst) == 2,
        "source scope escaped via root grant: same_instance={}, other_instance={}, provider_calls={}",
        same_instance.is_ok(),
        other_instance.is_ok(),
        seen.load(std::sync::atomic::Ordering::SeqCst)
    );
}

#[tokio::test]
async fn source_package_root_commands_cannot_escape_the_admitted_scope() {
    a_source_package_root_grant_does_not_replace_a_route_scope(true).await;
}

#[tokio::test]
async fn source_package_root_tools_cannot_escape_the_admitted_scope() {
    a_source_package_root_grant_does_not_replace_a_route_scope(false).await;
}

async fn submit_after_held_attachment_authority_change(
    changed: &str,
    queued_upload: bool,
) -> (usize, usize, usize) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_root, s, instance, resource, app) = fixture().await;
    let package = registry::active(&s, "router").await.unwrap();
    let before = "return cairn.conversations.submit({text = event.text,";
    assert_eq!(package.source.main.matches(before).count(), 1);
    let source = package.source.main.replace(before,
        "return cairn.conversations.submit({files = {{url = 'https://api.example.test/download', name = 'note.txt'}}, text = event.text,");
    let mut manifest = serde_json::to_value(&package.manifest).unwrap();
    manifest["version"] = json!("2");
    let installed = registry::install(&s, &json!({"manifest": manifest, "source": source}))
        .await
        .unwrap();
    registry::approve(&s, "router", "2", text(&installed, "digest"))
        .await
        .unwrap();
    let agent = s
        .agent(
            json!({
                "name": "Attachment reader",
                "access": {"projects": [], "installation": false, "github": false}
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
            "userIds": ["alice"]
        }),
    )
    .await;

    let owner_changed = Arc::new(AtomicBool::new(false));
    let revoked = owner_changed.clone();
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let access = if revoked.load(Ordering::SeqCst) {
                "owner-rejoined"
            } else {
                "fixture-owner"
            };
            async move {
                axum::Json(json!({
                    "owner": {"account_id": "fixture-owner", "access_id": access},
                    "members": []
                }))
            }
        }),
    );
    let (origin, authority_server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (entering, releasing) = (entered.clone(), release.clone());
    let provider = axum::Router::new().route(
        "/download",
        axum::routing::get(move || {
            let (entered, release) = (entering.clone(), releasing.clone());
            async move {
                entered.notify_one();
                release.notified().await;
                "Synthetic attachment content"
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let (status, body) = deliver(
        &app,
        &instance,
        json!({
            "id": "attachment-request",
            "resource": resource,
            "author": {"id": "alice"},
            "context": {"channel": "C1", "thread": "thread1"},
            "body": {"text": "Read my attachment"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "accepted");
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
    let upload_guard = if queued_upload {
        Some(s.attachment_upload.lock().await)
    } else {
        None
    };
    let calling = s.clone();
    let mut pending = tokio::spawn(async move {
        let result = runtime::pump(&calling, &connection, &mut session).await;
        session.stop().await;
        result
    });
    tokio::select! {
        () = entered.notified() => {},
        result = &mut pending => panic!("Ended before attachment I/O: {result:?}"),
        () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
            panic!("The attachment download was not reached")
        },
    }
    assert!(s.chat_list().await.unwrap().is_empty());
    if queued_upload {
        // The first authorized transaction creates the chat; publication of its
        // file must still wait behind the same quota lock as ordinary uploads.
        release.notify_one();
        common::eventually(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(10),
            async || (!s.chat_list().await.unwrap().is_empty()).then_some(()),
        )
        .await;
    }
    if changed == "owner" {
        owner_changed.store(true, Ordering::SeqCst);
    } else if changed == "approval" {
        let mut manifest = serde_json::to_value(&package.manifest).unwrap();
        manifest["version"] = json!("3");
        let replacement = registry::install(
            &s,
            &json!({
                "manifest": manifest,
                "source": format!("-- replacement package\n{}", package.source.main)
            }),
        )
        .await
        .unwrap();
        registry::approve(&s, "router", "3", text(&replacement, "digest"))
            .await
            .unwrap();
    }
    if !queued_upload {
        release.notify_one();
    }
    drop(upload_guard);
    let _ = pending.await.unwrap();
    let chats = s.chat_list().await.unwrap();
    let mut messages = 0;
    for chat in &chats {
        let detail = s.chat_detail(text(chat, "id")).await.unwrap();
        messages += detail["messages"].as_array().unwrap().len();
    }
    let attachments = s
        .store
        .read(|db| db.keys("chat-attachment:"))
        .await
        .unwrap()
        .len();
    server.abort();
    authority_server.abort();
    (chats.len(), messages, attachments)
}

#[tokio::test]
async fn owner_revocation_during_attachment_download_prevents_conversation_submission() {
    assert_eq!(
        submit_after_held_attachment_authority_change("control", false).await,
        (1, 1, 1),
        "The positive control never submitted the downloaded attachment"
    );
    assert_eq!(
        submit_after_held_attachment_authority_change("owner", false).await,
        (0, 0, 0),
        "A retired owner persisted a conversation after attachment I/O"
    );
}

#[tokio::test]
async fn approval_replacement_during_attachment_download_prevents_conversation_submission() {
    assert_eq!(
        submit_after_held_attachment_authority_change("control", false).await,
        (1, 1, 1),
        "The positive control never submitted the downloaded attachment"
    );
    assert_eq!(
        submit_after_held_attachment_authority_change("approval", false).await,
        (0, 0, 0),
        "A retired package persisted a conversation after attachment I/O"
    );
}

#[tokio::test]
async fn owner_revocation_while_an_attachment_waits_for_quota_prevents_publication() {
    assert_eq!(
        submit_after_held_attachment_authority_change("control", true).await,
        (1, 1, 1),
        "The positive control never published its queued attachment"
    );
    assert_eq!(
        submit_after_held_attachment_authority_change("owner", true).await,
        (1, 0, 0),
        "A retired owner published an attachment after waiting for quota"
    );
}

#[tokio::test]
async fn approval_replacement_while_an_attachment_waits_for_quota_prevents_publication() {
    assert_eq!(
        submit_after_held_attachment_authority_change("control", true).await,
        (1, 1, 1),
        "The positive control never published its queued attachment"
    );
    assert_eq!(
        submit_after_held_attachment_authority_change("approval", true).await,
        (1, 0, 0),
        "A retired package published an attachment after waiting for quota"
    );
}

#[tokio::test]
async fn everyone_routes_refuse_resource_grants_without_losing_admission() {
    let (_root, s, instance, resource, app) = fixture().await;
    let mut agents = Vec::new();
    for name in ["Public route agent", "Restricted agent"] {
        agents.push(s.agent(json!({
            "name": name,
            "access": {"sandbox": "read-only", "projects": [], "installation": false, "github": false, "mcps": []},
        }), None).await.unwrap());
    }
    let public = &agents[0];
    let restricted = &agents[1];
    let route = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Public channel", "enabled": true,
            "match": {"channel": "C1"}, "agentId": public["id"], "accessMode": "everyone",
        }),
    )
    .await;
    let path =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/grants");
    let grant = |agent: &Value| {
        owner_request(
            "POST",
            &path,
            json!({"agentId": agent["id"], "operations": ["read"]}),
        )
    };

    let response = common::send(&app, grant(restricted)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let response = common::send(&app, grant(public)).await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "An Everyone route must not be disabled by a new credential grant: {}",
        common::read_text(response).await,
    );

    let (status, result) = deliver(
        &app,
        &instance,
        json!({
            "id": "public-after-refused-grant", "resource": resource,
            "author": {"id": "alice"}, "context": {"channel": "C1", "thread": "public"},
            "body": {"text": "Hello"},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["status"], "accepted", "{result}");

    let route_path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}",
        text(&route, "id"),
    );
    let response = common::send(
        &app,
        owner_request(
            "DELETE",
            &route_path,
            json!({"revision": route["revision"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let response = common::send(&app, grant(public)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
}

struct QueuedRoutedRequest {
    _root: TempDir,
    s: Arc<Service>,
    instance: String,
    resource: String,
    app: axum::Router,
    route: Value,
    run: String,
}

async fn queued_routed_request() -> QueuedRoutedRequest {
    queued_routed_request_with_mode("requests").await
}

async fn queued_routed_request_with_mode(mode: &str) -> QueuedRoutedRequest {
    queued_routed_request_for_execution(mode, false).await
}

async fn queued_routed_request_for_execution(mode: &str, host: bool) -> QueuedRoutedRequest {
    let (_root, s, instance, resource, app) = if host {
        configured_fixture_with_runner(None, "").await
    } else {
        fixture().await
    };
    if !host {
        s.accounts
            .create(
                &s,
                cairn_installation::provider::Provider::Codex,
                "Not connected fixture",
            )
            .await
            .unwrap();
    }
    let mut access = json!({"projects": [], "installation": false, "github": false});
    if mode == "everyone" {
        access["sandbox"] = json!("read-only");
        access["mcps"] = json!([]);
    }
    let agent = s
        .agent(
            json!({"name": "Queued request fixture", "access": access}),
            None,
        )
        .await
        .unwrap();
    let route = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Engineering", "enabled": true, "match": { "channel": "C1" },
            "agentId": agent["id"], "accessMode": mode, "userIds": ["alice", "bob"],
        }),
    )
    .await;
    let admitted = deliver(
        &app,
        &instance,
        json!({
            "id": "queued-alice", "resource": resource,
            "author": { "id": "alice" }, "context": { "channel": "C1", "thread": "queued-thread" },
            "body": { "text": "Request queued before revocation" },
        }),
    )
    .await;
    assert_eq!(admitted.0, StatusCode::OK);
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
    runtime::begin(&s, &connection, &mut session).await.unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    session.stop().await;
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chats = s.chat_list().await.unwrap();
    assert_eq!(chats.len(), 1);
    let detail = s.chat_detail(text(&chats[0], "id")).await.unwrap();
    let id = text(&detail["run"], "id").to_owned();
    assert_eq!(detail["run"]["status"], "queued");
    // A real scheduler pass leaves a valid request waiting for unavailable capacity.
    // This fixture launches no agent process or VM.
    if !host {
        s.worker.tick(&s).await.unwrap();
    }
    let waiting = s.store.run(&id).await.unwrap();
    assert_eq!(waiting["status"], "queued");
    assert!(s.worker.active.lock().await.is_empty());
    QueuedRoutedRequest {
        _root,
        s,
        instance,
        resource,
        app,
        route,
        run: id,
    }
}

#[tokio::test]
async fn staged_routed_requests_lose_native_worker_admission_when_their_source_is_revoked() {
    let mut observed = Vec::new();
    for change in ["person", "reapprove", "route", "resource"] {
        let QueuedRoutedRequest {
            _root,
            s,
            instance,
            resource,
            app,
            route,
            run: id,
        } = queued_routed_request().await;
        let route_path = format!(
            "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}",
            text(&route, "id"),
        );
        match change {
            "person" | "reapprove" => {
                let response = common::send(
                    &app,
                    owner_request(
                        "POST",
                        &format!("{route_path}/people/alice"),
                        json!({ "action": "remove", "revision": route["revision"] }),
                    ),
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
                let removed = common::read_json(response).await;
                if change == "reapprove" {
                    let fresh = deliver(&app, &instance, json!({
                        "id": "new-access-request", "resource": resource,
                        "author": { "id": "alice" }, "context": { "channel": "C1", "thread": "fresh-thread" },
                        "body": { "text": "New permission request" },
                    })).await;
                    assert_eq!(fresh.1["status"], "requested");
                    let response = common::send(
                        &app,
                        owner_request(
                            "POST",
                            &format!("{route_path}/people/alice"),
                            json!({ "action": "approve", "revision": removed["revision"] }),
                        ),
                    )
                    .await;
                    assert_eq!(
                        response.status(),
                        StatusCode::OK,
                        "{}",
                        common::read_text(response).await
                    );
                }
            }
            "route" => {
                let response = common::send(&app, owner_request("PUT", &route_path, json!({
                    "name": "Engineering", "enabled": false, "match": { "channel": "C1" },
                    "agentId": route["agentId"], "userIds": ["alice", "bob"], "revision": route["revision"],
                }))).await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{}",
                    common::read_text(response).await
                );
            }
            "resource" => {
                let value = resources::get(&s, "router", &instance, "workspace", &resource)
                    .await
                    .unwrap();
                let response = common::send(&app, owner_request("DELETE", &format!(
                    "/api/plugins/router/connections/{instance}/resources/workspace/{resource}",
                ), json!({ "revision": value.revision }))).await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{}",
                    common::read_text(response).await
                );
            }
            _ => unreachable!(),
        }
        let result = s.worker.tick(&s).await;
        let current = s.store.run(&id).await.unwrap();
        observed.push((
            change,
            current["status"].clone(),
            result.err().map(|e| e.message),
        ));
        assert!(s.worker.active.lock().await.is_empty());
    }
    assert!(
        observed
            .iter()
            .all(|(_, status, error)| status == "cancelled" && error.is_none()),
        "Source withdrawal did not settle staged native work: {observed:?}"
    );
}

#[tokio::test]
async fn native_source_is_retained_across_owner_lookup_and_temporary_failures() {
    for change in [
        "person",
        "reapprove",
        "package",
        "same-package",
        "file",
        "unavailable",
    ] {
        let QueuedRoutedRequest {
            _root,
            s,
            instance,
            resource,
            app,
            route,
            run: id,
        } = queued_routed_request().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (entering, releasing) = (entered.clone(), release.clone());
        let unavailable = change == "unavailable";
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let first = first.swap(false, std::sync::atomic::Ordering::SeqCst);
                let (entered, release) = (entering.clone(), releasing.clone());
                async move {
                    if first {
                        entered.notify_one();
                        release.notified().await;
                        if unavailable {
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                "Synthetic authority unavailable",
                            )
                                .into_response();
                        }
                    }
                    axum::Json(json!({
                        "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                        "members": [],
                    }))
                    .into_response()
                }
            }),
        );
        let (origin, authority_server) = common::serve_locally(authority).await;
        let identity_path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        let saved_route_before = s
            .store
            .get("plugin-routes", text(&route, "id"))
            .await
            .unwrap();
        let service = s.clone();
        let ticking = tokio::spawn(async move { service.worker.tick(&service).await });
        tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        if change == "file" {
            std::fs::write(
                s.config.data_dir.join("plugins/router/1/main.luau"),
                "return { changed = true }",
            )
            .unwrap();
        } else if matches!(change, "package" | "same-package") {
            let package = registry::active(&s, "router").await.unwrap();
            if change == "same-package" {
                registry::approve(&s, "router", "1", &package.digest)
                    .await
                    .unwrap();
            } else {
                let mut manifest = serde_json::to_value(&package.manifest).unwrap();
                manifest["version"] = json!("2");
                let replacement = registry::install(
                    &s,
                    &json!({
                        "manifest": manifest,
                        "source": package.source.main,
                        "modules": package.source.modules,
                    }),
                )
                .await
                .unwrap();
                registry::approve(&s, "router", "2", text(&replacement, "digest"))
                    .await
                    .unwrap();
            }
            let saved_route = s
                .store
                .get("plugin-routes", text(&route, "id"))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(Some(saved_route), saved_route_before);
        } else if !unavailable {
            let route_path = format!(
                "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}",
                text(&route, "id"),
            );
            let response = common::send(
                &app,
                owner_request(
                    "POST",
                    &format!("{route_path}/people/alice"),
                    json!({"action": "remove", "revision": route["revision"]}),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let removed = common::read_json(response).await;
            if change == "reapprove" {
                let fresh = deliver(&app, &instance, json!({
                    "id": "new-source-access", "resource": resource,
                    "author": {"id": "alice"}, "context": {"channel": "C1", "thread": "new-source-thread"},
                    "body": {"text": "A new access request"},
                })).await;
                assert_eq!(fresh.1["status"], "requested");
                let response = common::send(
                    &app,
                    owner_request(
                        "POST",
                        &format!("{route_path}/people/alice"),
                        json!({"action": "approve", "revision": removed["revision"]}),
                    ),
                )
                .await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{}",
                    common::read_text(response).await
                );
            }
        }
        release.notify_one();
        ticking.await.unwrap().unwrap();
        let current = s.store.run(&id).await.unwrap();
        assert_eq!(
            current["status"],
            if unavailable { "queued" } else { "cancelled" },
            "{change}: {current}"
        );
        assert!(s.worker.active.lock().await.is_empty());
        if unavailable {
            assert_eq!(
                current["accountWaitReason"],
                "Waiting to verify the requester's source access."
            );
            s.worker.tick(&s).await.unwrap();
            let resumed = s.store.run(&id).await.unwrap();
            assert_eq!(resumed["status"], "queued");
            assert_ne!(resumed["accountWaitReason"], current["accountWaitReason"]);
        }
        authority_server.abort();
    }
}

#[tokio::test]
async fn a_new_instance_grant_retires_public_routed_work_without_stalling_the_scheduler() {
    let QueuedRoutedRequest {
        _root,
        s,
        instance,
        resource,
        app,
        route,
        run,
    } = queued_routed_request_with_mode("everyone").await;
    let installed = registry::install(&s, &json!({
        "manifest": {"id": "private-credential", "name": "Private credential", "version": "1", "apiVersion": 2,
            "capabilities": [], "agentAccess": {}},
        "source": "return {}",
    })).await.unwrap();
    registry::approve(&s, "private-credential", "1", text(&installed, "digest"))
        .await
        .unwrap();
    connections::save(
        &s,
        "private-credential",
        &json!({
            "name": "Private access", "enabled": true, "agentIds": [route["agentId"]],
        }),
        None,
    )
    .await
    .unwrap();
    let result = s.worker.tick(&s).await;
    let current = s.store.run(&run).await.unwrap();
    assert!(
        result.is_ok(),
        "A withdrawn public source aborted scheduling: {result:?}"
    );
    assert_eq!(current["status"], "cancelled");
    assert!(s.worker.active.lock().await.is_empty());
    // Revoked public eligibility also refuses new ingress instead of retaining
    // a reusable input-validation error or widening the route's permissions.
    let response = deliver(
        &app,
        &instance,
        json!({
            "id": "after-private-grant", "resource": resource, "author": {"id": "alice"},
            "context": {"channel": "C1", "thread": "fresh"}, "body": {"text": "Hello"},
        }),
    )
    .await;
    assert_eq!(response.0, StatusCode::FORBIDDEN);
}

async fn assert_invalid_package_does_not_stall_scheduler(change: &str) {
    let QueuedRoutedRequest {
        _root,
        s,
        route,
        run,
        ..
    } = queued_routed_request().await;
    let chat = s
        .chat_create(json!({"agentId": route["agentId"]}))
        .await
        .unwrap();
    s.chat_send(
        text(&chat, "id"),
        json!({"id": cairn_installation::config::id(), "text": "Independent ordinary request"}),
    )
    .await
    .unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    let control = text(&detail["run"], "id").to_owned();
    assert!(s.store.run(&control).await.unwrap()["accountWaitReason"].is_null());
    let directory = s.config.data_dir.join("plugins/router/1");
    match change {
        "json" => std::fs::write(directory.join("manifest.json"), "{").unwrap(),
        "missing-source" => std::fs::remove_file(directory.join("main.luau")).unwrap(),
        "manifest-utf8" => std::fs::write(directory.join("manifest.json"), [0xff]).unwrap(),
        "source-utf8" => std::fs::write(directory.join("main.luau"), [0xff]).unwrap(),
        _ => unreachable!(),
    }
    let result = s.worker.tick(&s).await;
    assert!(
        result.is_ok(),
        "An invalid integration aborted unrelated scheduling: {result:?}"
    );
    assert_eq!(s.store.run(&run).await.unwrap()["status"], "cancelled");
    let independent = s.store.run(&control).await.unwrap();
    assert_eq!(independent["status"], "queued");
    assert!(
        independent["accountWaitReason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "The independent run did not reach its capacity check: {independent}"
    );
    assert!(s.worker.active.lock().await.is_empty());
}

#[tokio::test]
async fn an_invalid_admitted_package_does_not_block_an_unrelated_queued_run() {
    assert_invalid_package_does_not_stall_scheduler("json").await;
}

#[tokio::test]
async fn a_missing_admitted_source_file_does_not_block_an_unrelated_queued_run() {
    assert_invalid_package_does_not_stall_scheduler("missing-source").await;
}

#[tokio::test]
async fn invalid_utf8_admitted_package_files_do_not_block_an_unrelated_queued_run() {
    for change in ["manifest-utf8", "source-utf8"] {
        assert_invalid_package_does_not_stall_scheduler(change).await;
    }
}

#[tokio::test]
async fn revoking_a_person_during_a_question_keeps_other_routes_running() {
    let (_root, s, instance, resource, app) = fixture().await;
    install_question_handler(&s).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let platform = axum::Router::new()
        .route(
            "/question-result",
            axum::routing::post({
                let (entered, release) = (entered.clone(), release.clone());
                move || {
                    let (entered, release) = (entered.clone(), release.clone());
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        axum::Json(json!({"ok": true}))
                    }
                }
            }),
        )
        .route(
            "/reply-result",
            axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
                sent.send(value).unwrap();
                async { axum::Json(json!({"ok": true})) }
            }),
        );
    let (origin, server) = common::serve_locally(platform).await;
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
    let mut alice_route = Value::Null;
    for (channel, person) in [("C1", "alice"), ("C2", "bob")] {
        let route = create_route(
            &app,
            &instance,
            &resource,
            json!({
                "name": channel,
                "enabled": true,
                "match": {"channel": channel},
                "agentId": agent["id"],
                "userIds": [person],
            }),
        )
        .await;
        if person == "alice" {
            alice_route = route;
        }
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
    let request = |person: &str, channel: &str| {
        json!({
            "id": format!("question-race-{person}"),
            "resource": resource,
            "author": {"id": person},
            "context": {"channel": channel, "thread": person},
            "body": {"text": "Work in my own thread", "trace": true},
        })
    };
    assert_eq!(
        deliver(&app, &instance, request("alice", "C1")).await.1["status"],
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
    s.chat_acknowledge(text(&run, "id"), text(&run["chatExecution"], "messageId"))
        .await
        .unwrap();
    s.question_receive(
        text(&run, "id"),
        json!({
            "id": "d".repeat(64),
            "blocking": false,
            "fields": [{"id": "choice", "title": "Continue?", "options": []}],
        }),
    )
    .await
    .unwrap();
    let mut delivery = s.store.list("plugin-deliveries").await.unwrap()[0].clone();
    delivery["progressAt"] = json!(now() - 9_000);
    s.store.put("plugin-deliveries", delivery).await.unwrap();
    let pump = tokio::spawn({
        let (s, connection) = (s.clone(), connection.clone());
        async move {
            let result = runtime::pump(&s, &connection, &mut session).await;
            (session, result)
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/alice",
        text(&alice_route, "id"),
    );
    let removed = common::send(
        &app,
        owner_request(
            "POST",
            &path,
            json!({
                "action": "remove",
                "revision": alice_route["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(removed.status(), StatusCode::OK);
    release.notify_one();
    let (mut session, result) = pump.await.unwrap();
    assert!(
        result.is_ok(),
        "One person's question/progress revocation stopped the shared worker: {result:?}"
    );
    assert!(session.alive());
    assert!(s.store.list("plugin-deliveries").await.unwrap().is_empty());

    assert_eq!(
        deliver(&app, &instance, request("bob", "C2")).await.1["status"],
        "accepted"
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let delivery = s.store.list("plugin-deliveries").await.unwrap()[0].clone();
    let chat = s.get("chats", text(&delivery, "chatId")).await.unwrap();
    let run = text(&chat, "runId");
    s.chat_acknowledge(run, text(&delivery, "id"))
        .await
        .unwrap();
    s.store
        .event(
            run,
            "item.completed",
            "",
            Some(json!({
                "item": {"type": "agent_message", "text": "Bob's own result"},
            })),
        )
        .await
        .unwrap();
    s.store
        .event(run, "turn.completed", "", None)
        .await
        .unwrap();
    s.store
        .patch_run(run, json!({"status": "succeeded"}))
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(3), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply["userId"], "bob");
    assert_eq!(reply["text"], "Bob's own result");
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn approval_replacement_during_ingress_authority_check_keeps_no_content_or_dedupe() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Dedicated",
                "access": {
                    "projects": [],
                    "installation": false,
                    "github": false,
                },
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
            "name": "Support",
            "enabled": true,
            "match": { "channel": "C1" },
            "agentId": agent["id"],
            "accessMode": "allowlist",
            "userIds": ["alice"],
        }),
    )
    .await;
    let package = registry::active(&s, "router").await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (entering, releasing) = (entered.clone(), release.clone());
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let (entered, release) = (entering.clone(), releasing.clone());
            // Route selection checks the owner first. Hold only deliver's final
            // membership lookup, after its old package digest has been checked.
            let hold = calls.fetch_add(1, Ordering::SeqCst) == 1;
            async move {
                if hold {
                    entered.notify_one();
                    release.notified().await;
                }
                axum::Json(json!({
                    "owner": {
                        "account_id": "fixture-owner",
                        "access_id": "fixture-owner",
                    },
                    "members": []
                }))
            }
        }),
    );
    let (origin, server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let (calling, target, resource_id) = (app.clone(), instance.clone(), resource.clone());
    let mut pending = tokio::spawn(async move {
        deliver(
            &calling,
            &target,
            json!({
                "id": "retired-ingress",
                "resource": resource_id,
                "author": { "id": "alice" },
                "context": {
                    "channel": "C1",
                    "thread": "private-thread",
                },
                "body": { "text": "This rejected content must not be retained" },
            }),
        )
        .await
    });
    tokio::select! {
        () = entered.notified() => {},

        result = &mut pending => panic!("Ingress ended before final authority lookup: {result:?}"),

        () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
            panic!("Ingress did not reach final authority lookup")
        },
    }
    let mut manifest = serde_json::to_value(&package.manifest).unwrap();
    manifest["version"] = json!("2");
    let replacement = registry::install(
        &s,
        &json!({
            "manifest": manifest,
            "source": format!("-- replacement\n{}", package.source.main),
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "router", "2", text(&replacement, "digest"))
        .await
        .unwrap();
    release.notify_one();
    let (status, _) = pending.await.unwrap();
    server.abort();
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "Retired ingress must be rejected by approval, not a timeout"
    );
    assert!(
        s.store.list("plugin-events").await.unwrap().is_empty(),
        "Rejected ingress persisted its message content",
    );
    let dedupe = format!("plugin-seen:{instance}:{resource}:retired-ingress");
    assert!(
        s.store.kv(&dedupe).await.unwrap().is_none(),
        "Rejected ingress consumed its event ID"
    );
}

#[tokio::test]
async fn preparing_routed_runs_retry_authority_outages_without_owner_resume() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // Fail before mark_running, then after access preparation has started.
    for outage_after in [2, 3] {
        let QueuedRoutedRequest { _root, s, run, .. } =
            queued_routed_request_for_execution("requests", true).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let restored = Arc::new(AtomicBool::new(false));
        let resumed_calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (observed, available) = (calls.clone(), restored.clone());
        let (preparing, entering, releasing) =
            (resumed_calls.clone(), entered.clone(), release.clone());
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let call = observed.fetch_add(1, Ordering::SeqCst);
                let restored = available.load(Ordering::SeqCst);
                let resumed = restored && preparing.fetch_add(1, Ordering::SeqCst) >= 2;
                let (entered, release) = (entering.clone(), releasing.clone());
                async move {
                    if call >= outage_after && !restored {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Synthetic preparation outage",
                        )
                            .into_response();
                    }
                    let access = if resumed {
                        entered.notify_one();
                        release.notified().await;
                        "owner-rejoined"
                    } else {
                        "fixture-owner"
                    };
                    axum::Json(json!({
                        "owner": {"account_id": "fixture-owner", "access_id": access},
                        "members": [],
                    }))
                    .into_response()
                }
            }),
        );
        let (origin, authority_server) = common::serve_locally(authority).await;
        let identity_path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        s.worker.tick(&s).await.unwrap();
        common::eventually(
            std::time::Duration::from_secs(10),
            std::time::Duration::from_millis(10),
            async || {
                (calls.load(Ordering::SeqCst) > outage_after
                    && s.worker.active.lock().await.is_empty())
                .then_some(())
            },
        )
        .await;
        let waiting = s.store.run(&run).await.unwrap();
        assert_eq!(
            waiting["status"], "queued",
            "Preparation outage lost admitted work: {waiting}"
        );
        assert_eq!(
            waiting["accountWaitReason"],
            "Waiting to verify the requester's source access."
        );
        assert!(waiting["error"].is_null());

        // Ordinary admission reaches launch again for the same run without an
        // owner resume. Hold that launch check before any agent process can run.
        restored.store(true, Ordering::SeqCst);
        s.worker.tick(&s).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        assert!(s.worker.active.lock().await.contains_key(&run));
        assert_eq!(resumed_calls.load(Ordering::SeqCst), 3);
        // A fresh owner generation still revokes the preparing run's old proof;
        // recovery must not replace its original admitted authority.
        release.notify_one();
        let cancelled = common::eventually(
            std::time::Duration::from_secs(10),
            std::time::Duration::from_millis(10),
            async || {
                let current = s.store.run(&run).await.unwrap();
                (current["status"] == "cancelled" && s.worker.active.lock().await.is_empty())
                    .then_some(current)
            },
        )
        .await;
        authority_server.abort();
        assert_eq!(cancelled["id"], run);
        assert_eq!(s.chat_list().await.unwrap().len(), 1);
    }
}
