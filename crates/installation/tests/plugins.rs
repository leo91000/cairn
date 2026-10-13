mod common;

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use cairn_installation::{
    config::{MAIN_AGENT_ID, now},
    plugins::{
        connections::{self, Connection},
        discord, registry, runtime, work,
    },
    service::Service,
    validation::text,
};
use cairn_plugin_api::Manifest;
use cairn_plugin_host::{Denied, Host, Session};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::Mutex;

fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cairn"))
}

async fn service() -> (TempDir, Arc<Service>, Value) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".into();
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins.use_worker(worker());
    let agent = s
        .agent(
            json!({
                "name": "Discord agent",
                "access": { "projects": [], "skills": [], "mcps": [], "github": false, "installation": false },
            }),
            None,
        )
        .await
        .unwrap();
    (root, s, agent)
}

async fn fixture() -> (TempDir, Arc<Service>, Connection) {
    let (root, s, agent) = service().await;
    let connection = connections::save(
        &s,
        "discord",
        &json!({
            "name": "Test Discord",
            "settings": { "guildId": "100", "channelId": "200", "botName": "Cairn" },
            "userIds": ["300"],
            "agentId": agent["id"],
            "projectId": null,
            "maxMessagesPerHour": 2,
            "enabled": true,
            "token": "synthetic_discord_token",
        }),
        None,
    )
    .await
    .unwrap();
    (root, s, serde_json::from_value(connection).unwrap())
}

fn message(id: &str, author: &str, content: &str) -> Value {
    json!({
        "id": id,
        "guild_id": "100",
        "channel_id": "200",
        "content": content,
        "author": { "id": author, "username": "sarah", "global_name": "Sarah", "bot": false },
        "mentions": [{ "id": "400" }],
    })
}

async fn accept(
    s: &Service,
    connection: &Connection,
    value: Value,
) -> cairn_installation::error::Result<bool> {
    match discord::inbound(connection, "400", &value) {
        Some(inbound) => work::accept(s, connection, inbound).await,
        None => Ok(false),
    }
}

async fn session(s: &Service, plugin: &str) -> Session {
    let package = registry::active(s, plugin).await.unwrap();
    s.plugins
        .host
        .start(&worker(), &package.manifest, &package.source)
        .await
        .unwrap()
}

/// Records requests for any path and answers from a script of statuses.
#[derive(Default)]
struct Mock {
    requests: Mutex<Vec<(String, HeaderMap, Value)>>,
    statuses: Mutex<Vec<u16>>,
}

async fn record(
    State(mock): State<Arc<Mock>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    mock.requests
        .lock()
        .await
        .push((uri.path().to_owned(), headers, body));
    let status = mock.statuses.lock().await.pop().unwrap_or(200);
    match status {
        429 => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "retry_after": 1 })),
        )
            .into_response(),
        302 => (
            StatusCode::FOUND,
            [("location", "http://127.0.0.1:9/elsewhere")],
        )
            .into_response(),
        _ => Json(json!({ "id": "600" })).into_response(),
    }
}

async fn mock(s: &Service) -> (Arc<Mock>, tokio::task::JoinHandle<()>) {
    let mock = Arc::new(Mock::default());
    let app = Router::new()
        .route("/api/v10/channels/{channel}/messages", post(record))
        .route("/v1/ping", post(record))
        .route("/v1/redirect", get(record))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    s.plugins
        .route_http_to(Some(format!("http://{}", listener.local_addr().unwrap())));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (mock, server)
}

/// Completes the conversation turn for a delivery, as the agent worker would.
async fn complete(s: &Service, delivery: &Value, answer: &str) {
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.get("chats", text(delivery, "chatId")).await.unwrap();
    let run_id = text(&chat, "runId").to_owned();
    s.store
        .event(
            &run_id,
            "chat.user",
            "",
            Some(json!({ "messageId": delivery["id"] })),
        )
        .await
        .unwrap();
    s.store
        .event(
            &run_id,
            "item.completed",
            answer,
            Some(json!({ "item": { "type": "agent_message", "text": answer } })),
        )
        .await
        .unwrap();
    s.store
        .event(&run_id, "turn.completed", "", None)
        .await
        .unwrap();
}

#[tokio::test]
async fn worker_sessions_enforce_capabilities_deadlines_and_keep_state() {
    let host = Host::default();
    let onepassword = registry::builtin("onepassword").unwrap();
    let value = host
        .invoke(
            &worker(),
            &onepassword.manifest,
            &onepassword.source,
            json!({ "operation": "read" }),
            |method, input| async move {
                assert_eq!(method, "onepassword.read");
                assert_eq!(input["operation"], "read");
                Ok(json!({ "value": "synthetic_secret" }))
            },
        )
        .await
        .unwrap();
    assert_eq!(value["value"], "synthetic_secret");

    let calls = AtomicUsize::new(0);
    let error = host
        .invoke(
            &worker(),
            &onepassword.manifest,
            "return { command = function(input) return cairn.call('native.execute', input) end }",
            Value::Null,
            |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok(Value::Null) }
            },
        )
        .await
        .unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(error.contains("capability"));
    let error = host
        .invoke(
            &worker(),
            &onepassword.manifest,
            "return { command = function(input) error(input.secret) end }",
            json!({ "secret": "synthetic_secret" }),
            |_, _| async { unreachable!() },
        )
        .await
        .unwrap_err();
    assert!(!error.contains("synthetic_secret"));
    let mut incompatible = onepassword.manifest.clone();
    incompatible.api_version += 1;
    assert!(
        host.start(&worker(), &incompatible, &onepassword.source)
            .await
            .is_err()
    );

    let manifest: Manifest = serde_json::from_value(json!({
        "id": "probe", "name": "Probe", "version": "1", "apiVersion": 1, "capabilities": ["storage"]
    }))
    .unwrap();
    let mut session = host
        .start(
            &worker(),
            &manifest,
            "local count = 0 return { tick = function() count += 1 return count end, ask = function() local ok, code = pcall(cairn.storage.get, 'x') return tostring(code) end }",
        )
        .await
        .unwrap();
    for expected in [1, 2] {
        let value = session
            .dispatch("tick", Value::Null, |_, _| async { unreachable!() })
            .await
            .unwrap();
        assert_eq!(value, expected);
    }
    let value = session
        .dispatch("ask", Value::Null, |_, _| async {
            Err(Denied("forbidden"))
        })
        .await
        .unwrap();
    assert!(value.as_str().unwrap().contains("forbidden"));
    assert!(session.alive());
    session.stop().await;

    // A stuck handler ends its worker and releases the slot, even if it catches interrupts.
    let host = Host::with_limits(1, Duration::from_millis(300));
    let mut stuck = host
        .start(
            &worker(),
            &manifest,
            "return { spin = function() while true do pcall(function() while true do end end) end end }",
        )
        .await
        .unwrap();
    let started = std::time::Instant::now();
    // Either the worker's interrupt or the host's deadline ends it, never the script.
    let error = stuck
        .dispatch("spin", Value::Null, |_, _| async { unreachable!() })
        .await
        .unwrap_err();
    assert!(
        error.contains("deadline") || error.contains("failed"),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    stuck.stop().await;
    let mut next = host
        .start(
            &worker(),
            &manifest,
            "return { ok = function() return true end }",
        )
        .await
        .unwrap();
    assert_eq!(
        next.dispatch("ok", Value::Null, |_, _| async { unreachable!() })
            .await
            .unwrap(),
        true
    );
    next.stop().await;
}

#[tokio::test]
async fn discord_script_submits_mentions_and_delivers_chunked_replies() {
    let (_root, s, connection) = fixture().await;
    let (mock, server) = mock(&s).await;
    for (id, content) in [("498", "<@400>   "), ("499", "no mention")] {
        let mut value = message(id, "300", content);
        if id == "499" {
            value["mentions"] = json!([]);
        }
        accept(&s, &connection, value).await.unwrap();
    }
    assert!(
        accept(&s, &connection, message("500", "300", "<@400> hello"))
            .await
            .unwrap()
    );
    // Replays from the Gateway deduplicate.
    assert!(
        !accept(&s, &connection, message("500", "300", "<@400> hello"))
            .await
            .unwrap()
    );
    let mut session = session(&s, "discord").await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(s.store.list(work::EVENTS).await.unwrap().is_empty());
    let deliveries = s.store.list(work::DELIVERIES).await.unwrap();
    assert_eq!(deliveries.len(), 1, "the empty mention starts nothing");
    let delivery = deliveries[0].clone();
    assert_eq!(
        delivery["reply"],
        json!({ "channel": "200", "message": "500" })
    );
    let answer = format!("{}{}", "é".repeat(1900), "done");
    complete(&s, &delivery, &answer).await;

    mock.statuses.lock().await.push(429);
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let mut waiting = s.store.list(work::DELIVERIES).await.unwrap().remove(0);
    assert!(waiting["nextAttemptAt"].as_i64().unwrap() > now());
    waiting["nextAttemptAt"] = json!(now());
    s.store.put(work::DELIVERIES, waiting).await.unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(s.store.list(work::DELIVERIES).await.unwrap().is_empty());
    let activity = s.store.list("plugin-activity").await.unwrap();
    assert_eq!(activity[0]["status"], "replied");
    assert_eq!(activity[0]["preview"], "hello");

    let requests = mock.requests.lock().await;
    assert_eq!(requests.len(), 3);
    for (path, headers, body) in requests.iter() {
        assert_eq!(path, "/api/v10/channels/200/messages");
        assert_eq!(headers["authorization"], "Bot synthetic_discord_token");
        assert!(
            headers["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("DiscordBot")
        );
        assert_eq!(body["allowed_mentions"]["parse"], json!([]));
        assert_eq!(body["enforce_nonce"], true);
    }
    assert_eq!(requests[0].2["nonce"], requests[1].2["nonce"]);
    assert_ne!(requests[1].2["nonce"], requests[2].2["nonce"]);
    assert_eq!(requests[1].2["message_reference"]["message_id"], "500");
    assert!(requests[2].2["message_reference"].is_null());
    let sent = format!(
        "{}{}",
        text(&requests[1].2, "content"),
        text(&requests[2].2, "content")
    );
    assert_eq!(sent, answer);
    drop(requests);
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn access_requests_wait_for_approval_without_storing_content() {
    let (_root, s, connection) = fixture().await;
    let (mock, server) = mock(&s).await;
    let saved = connections::save(
        &s,
        "discord",
        &json!({
            "name": "#support on Acme",
            "settings": { "guildId": "100", "channelId": "200" },
            "accessMode": "requests",
            "userIds": [],
            "agentId": connection.agent_id,
            "projectId": null,
            "enabled": true,
        }),
        Some(&connection.id),
    )
    .await
    .unwrap();
    let connection: Connection = serde_json::from_value(saved).unwrap();
    let mut session = session(&s, "discord").await;
    for id in ["600", "601"] {
        assert!(
            !accept(
                &s,
                &connection,
                message(id, "301", "<@400> private question")
            )
            .await
            .unwrap()
        );
    }
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(
        !accept(
            &s,
            &connection,
            message("602", "301", "<@400> private question")
        )
        .await
        .unwrap()
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let requests = s.store.list("plugin-requests").await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["name"], "Sarah");
    assert_eq!(requests[0]["mentions"], 3);
    // The script answered once, and nothing the person wrote was kept anywhere.
    assert_eq!(mock.requests.lock().await.len(), 1);
    assert!(text(&mock.requests.lock().await[0].2, "content").contains("approve"));
    for kind in [
        "plugin-requests",
        "plugin-events",
        "plugin-storage",
        "plugin-activity",
    ] {
        let stored = serde_json::to_string(&s.store.list(kind).await.unwrap()).unwrap();
        assert!(!stored.contains("private question"), "{kind}");
    }

    // Approval applies to the running worker without a new revision.
    connections::people(&s, "discord", &connection.id, "301", "approve")
        .await
        .unwrap();
    assert!(
        accept(&s, &connection, message("603", "301", "<@400> now allowed"))
            .await
            .unwrap()
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(s.store.list(work::DELIVERIES).await.unwrap().len(), 1);

    assert!(
        !accept(&s, &connection, message("610", "302", "<@400> hi"))
            .await
            .unwrap()
    );
    connections::people(&s, "discord", &connection.id, "302", "deny")
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let notices = mock.requests.lock().await.len();
    assert!(
        !accept(&s, &connection, message("611", "302", "<@400> hi"))
            .await
            .unwrap()
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        mock.requests.lock().await.len(),
        notices,
        "denied people stay silent"
    );

    let overview = connections::overview(&s).await.unwrap();
    assert_eq!(overview[0]["userIds"], json!(["301"]));
    assert_eq!(
        overview[0]["people"],
        json!([{ "id": "301", "name": "Sarah" }])
    );
    assert_eq!(overview[0]["requests"], json!([]));
    assert_eq!(overview[0]["usage"]["hour"], 1);
    assert_eq!(overview[0]["activity"][0]["name"], "Sarah");

    connections::people(&s, "discord", &connection.id, "301", "remove")
        .await
        .unwrap();
    connections::set_enabled(&s, "discord", &connection.id, &json!({ "enabled": false }))
        .await
        .unwrap();
    assert_eq!(
        accept(&s, &connection, message("605", "301", "<@400> hi"))
            .await
            .unwrap_err()
            .status,
        403
    );
    connections::remove(&s, "discord", &connection.id)
        .await
        .unwrap();
    for kind in connections::OWNED {
        assert!(s.store.list(kind).await.unwrap().is_empty(), "{kind}");
    }
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn rate_limits_and_changed_agent_access_stop_admission() {
    let (_root, s, connection) = fixture().await;
    let event = |id: &str| {
        json!({
            "name": "message",
            "revision": connection.revision,
            "authorId": "300",
            "payload": { "id": id, "author": { "id": "300", "name": "Sarah" } },
        })
    };
    for id in ["500", "501"] {
        work::submit(&s, &connection, &event(id), &json!({ "text": "hello" }))
            .await
            .unwrap();
    }
    assert_eq!(
        work::submit(
            &s,
            &connection,
            &event("502"),
            &json!({ "text": "too many" })
        )
        .await
        .unwrap_err()
        .status,
        429
    );
    // Scripts can't act for someone else or outside a message.
    let mut stranger = event("503");
    stranger["authorId"] = json!("999");
    assert_eq!(
        work::submit(&s, &connection, &stranger, &json!({ "text": "x" }))
            .await
            .unwrap_err()
            .status,
        403
    );
    let mut timer = event("504");
    timer["name"] = json!("timer");
    assert_eq!(
        work::submit(&s, &connection, &timer, &json!({ "text": "x" }))
            .await
            .unwrap_err()
            .status,
        403
    );

    let mut agent = s.get("agents", &connection.agent_id).await.unwrap();
    agent["access"]["projects"] = Value::Null;
    s.agent(agent, Some(&connection.agent_id)).await.unwrap();
    assert_eq!(
        accept(&s, &connection, message("505", "300", "<@400> hi"))
            .await
            .unwrap_err()
            .status,
        403
    );
    let broad = connections::save(
        &s,
        "discord",
        &json!({
            "name": "Broad",
            "settings": { "guildId": "100", "channelId": "200" },
            "userIds": ["300"],
            "agentId": MAIN_AGENT_ID,
            "enabled": true,
            "token": "synthetic_discord_token",
        }),
        None,
    )
    .await;
    assert!(broad.is_err());
    for settings in [
        json!({ "guildId": "100" }),
        json!({ "guildId": "100", "channelId": "x" }),
        json!({ "guildId": "100", "channelId": "200", "extra": 1 }),
    ] {
        let result = connections::save(
            &s,
            "discord",
            &json!({ "name": "Bad", "settings": settings, "userIds": ["300"], "agentId": connection.agent_id, "enabled": false, "token": "t" }),
            None,
        )
        .await;
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn replies_export_only_the_correlated_completed_turn_and_revoke_immediately() {
    let (_root, s, connection) = fixture().await;
    let event = json!({
        "name": "message",
        "revision": connection.revision,
        "authorId": "300",
        "payload": { "id": "500", "author": { "id": "300", "name": "Sarah" } },
    });
    let receipt = work::submit(&s, &connection, &event, &json!({ "text": "hello" }))
        .await
        .unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.get("chats", text(&receipt, "chatId")).await.unwrap();
    let run_id = text(&chat, "runId");
    let agent = |text: &str| Some(json!({ "item": { "type": "agent_message", "text": text } }));
    s.store
        .event(
            run_id,
            "item.completed",
            "unrelated answer",
            agent("unrelated answer"),
        )
        .await
        .unwrap();
    s.store
        .event(run_id, "turn.completed", "", None)
        .await
        .unwrap();
    let delivery = s.store.list(work::DELIVERIES).await.unwrap().remove(0);
    assert!(
        work::reply(&s, &connection, &delivery)
            .await
            .unwrap()
            .is_none()
    );
    s.store
        .event(
            run_id,
            "chat.user",
            "hello",
            Some(json!({ "messageId": receipt["messageId"] })),
        )
        .await
        .unwrap();
    s.store
        .event(
            run_id,
            "item.completed",
            "synthetic_secret",
            Some(json!({ "item": { "type": "command_execution", "aggregated_output": "synthetic_secret" } })),
        )
        .await
        .unwrap();
    s.store
        .event(run_id, "item.completed", "hello back", agent("hello back"))
        .await
        .unwrap();
    assert!(
        work::reply(&s, &connection, &delivery)
            .await
            .unwrap()
            .is_none()
    );
    s.store
        .event(run_id, "turn.completed", "", None)
        .await
        .unwrap();
    assert_eq!(
        work::reply(&s, &connection, &delivery)
            .await
            .unwrap()
            .unwrap(),
        "hello back"
    );
    connections::remove(&s, "discord", &connection.id)
        .await
        .unwrap();
    assert_eq!(
        work::reply(&s, &connection, &delivery)
            .await
            .unwrap_err()
            .status,
        403
    );
    assert!(s.store.list(work::DELIVERIES).await.unwrap().is_empty());
    assert!(s.get("chats", text(&receipt, "chatId")).await.is_ok());
}

fn pinger(version: &str, capabilities: &[&str]) -> Value {
    json!({
        "manifest": {
            "id": "pinger",
            "name": "Pinger",
            "version": version,
            "description": "Pings a status API.",
            "apiVersion": 1,
            "capabilities": capabilities,
            "hosts": ["api.example.com"],
            "credential": { "label": "API key", "header": "x-api-key" },
            "settings": [{ "key": "team", "title": "Team", "type": "string", "required": true }],
        },
        "source": r#"return {
            start = function(event)
                cairn.timers.after(60, "ping", { team = "core" })
            end,
            timer = function(event)
                local response = cairn.http.request({ method = "POST", url = "https://api.example.com/v1/ping", json = event.payload, auth = true })
                cairn.storage.set("status", response.status)
                local ok, code = pcall(cairn.http.request, { url = "https://evil.example.org/collect" })
                cairn.storage.set("blocked", tostring(code))
                cairn.storage.set("redirect", cairn.http.request({ url = "https://api.example.com/v1/redirect" }).status)
            end,
        }"#,
    })
}

#[tokio::test]
async fn installed_packages_need_approval_and_reach_only_declared_hosts() {
    let (root, s, _) = service().await;
    let (mock, server) = mock(&s).await;
    for invalid in [
        json!({ "manifest": pinger("1.0.0", &["onepassword.read"])["manifest"], "source": "return {}" }),
        json!({ "manifest": { "id": "discord", "name": "Fake", "version": "1", "apiVersion": 1, "capabilities": [] }, "source": "return {}" }),
        json!({ "manifest": { "id": "broken", "name": "Broken", "version": "1", "apiVersion": 1, "capabilities": [] }, "source": "return function() end" }),
    ] {
        assert!(registry::install(&s, &invalid).await.is_err());
    }
    let package = pinger("1.0.0", &["timers", "storage", "http"]);
    registry::install(&s, &package).await.unwrap();
    let catalog = registry::catalog(&s).await.unwrap();
    let entry = catalog["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "pinger")
        .unwrap()
        .clone();
    assert_eq!(entry["status"], "needs_approval");
    let input = json!({ "name": "Core status", "settings": { "team": "core" }, "token": "secret-key", "enabled": true });
    assert_eq!(
        connections::save(&s, "pinger", &input, None)
            .await
            .unwrap_err()
            .status,
        403
    );

    registry::approve(&s, "pinger", "1.0.0").await.unwrap();
    for bad in [json!({ "team": "core", "other": 1 }), json!({})] {
        let mut input = input.clone();
        input["settings"] = bad;
        assert!(connections::save(&s, "pinger", &input, None).await.is_err());
    }
    let connection: Connection =
        serde_json::from_value(connections::save(&s, "pinger", &input, None).await.unwrap())
            .unwrap();
    assert!(
        connection.agent_id.is_empty(),
        "plugins without conversations get no agent"
    );

    let mut session = session(&s, "pinger").await;
    runtime::begin(&s, &connection, &mut session).await.unwrap();
    let mut timer = s.store.list("plugin-timers").await.unwrap().remove(0);
    assert!(timer["at"].as_i64().unwrap() > now() + 50_000);
    timer["at"] = json!(now());
    s.store.put("plugin-timers", timer).await.unwrap();
    mock.statuses.lock().await.extend([302, 200]);
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let requests = mock.requests.lock().await;
    assert_eq!(requests.len(), 2, "the undeclared host was never contacted");
    assert_eq!(requests[0].0, "/v1/ping");
    assert_eq!(requests[0].1["x-api-key"], "secret-key");
    assert_eq!(requests[0].2, json!({ "team": "core" }));
    assert!(requests[1].1.get("x-api-key").is_none());
    drop(requests);
    let stored = s.store.list("plugin-storage").await.unwrap();
    let value = |key: &str| stored.iter().find(|item| item["key"] == key).unwrap()["value"].clone();
    assert_eq!(value("status"), 200);
    assert!(value("blocked").as_str().unwrap().contains("forbidden"));
    assert_eq!(
        value("redirect"),
        302,
        "redirects are returned, never followed"
    );
    session.stop().await;

    // Editing an approved package on disk withdraws it until it is approved again.
    let source = root.path().join("data/plugins/pinger/1.0.0/main.luau");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(&source, "return { start = function() end }").unwrap();
    assert_eq!(
        registry::active(&s, "pinger").await.err().unwrap().status,
        403
    );
    std::fs::write(&source, &original).unwrap();
    assert!(registry::active(&s, "pinger").await.is_ok());

    // A new version waits for approval; the owner can roll back afterwards.
    registry::install(
        &s,
        &pinger(
            "1.1.0",
            &["timers", "storage", "http", "conversations.submit"],
        ),
    )
    .await
    .unwrap();
    let catalog = registry::catalog(&s).await.unwrap();
    let entry = catalog["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "pinger")
        .unwrap()
        .clone();
    assert_eq!(entry["status"], "update");
    assert_eq!(entry["active"], "1.0.0");
    assert!(registry::activate(&s, "pinger", "1.1.0").await.is_err());
    registry::approve(&s, "pinger", "1.1.0").await.unwrap();
    assert_eq!(
        registry::active(&s, "pinger")
            .await
            .unwrap()
            .manifest
            .version,
        "1.1.0"
    );
    registry::activate(&s, "pinger", "1.0.0").await.unwrap();
    assert_eq!(
        registry::active(&s, "pinger")
            .await
            .unwrap()
            .manifest
            .version,
        "1.0.0"
    );

    assert_eq!(
        registry::uninstall(&s, "pinger").await.unwrap_err().status,
        409
    );
    connections::remove(&s, "pinger", &connection.id)
        .await
        .unwrap();
    registry::uninstall(&s, "pinger").await.unwrap();
    assert!(!root.path().join("data/plugins/pinger").exists());
    assert!(registry::uninstall(&s, "discord").await.is_err());
    server.abort();
}

#[tokio::test]
async fn plugin_management_is_owner_only_and_mcp_never_returns_secret_reads() {
    let (_root, s, _connection) = fixture().await;
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let response = common::send(
        &app,
        common::request("GET", "/api/plugins")
            .extension(cairn_installation::auth::InstallationIdentity::trusted(
                cairn_installation::auth::InstallationRole::Member,
                "member",
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let task = s
        .task(
            json!({ "name": "Inspect", "agentId": MAIN_AGENT_ID, "prompt": "Inspect" }),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&run, "id"),
            json!({ "status": "running", "isolated": true }),
        )
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    let token = text(&config["env"], "CAIRN_MCP_RUN_TOKEN");
    let response = common::send(
        &app,
        common::request("POST", "/mcp-workspace")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": { "name": "onepassword", "arguments": { "operation": "read" } }
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    let response = common::read_json(response).await;
    assert!(
        response["result"]["isError"] == true || response["error"].is_object(),
        "{response}"
    );
    assert!(response["result"]["structuredContent"].is_null());
    let response = common::send(
        &app,
        common::request("POST", "/plugin-command")
            .header("authorization", "Bearer invalid")
            .body(Body::from(
                json!({ "plugin": "onepassword", "arguments": { "operation": "read" } })
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let catalog = s.store.list(connections::KIND).await.unwrap();
    assert!(
        !serde_json::to_string(&catalog)
            .unwrap()
            .contains("synthetic_discord_token")
    );
}
