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
        panels, registry, runtime, socket, work,
    },
    service::Service,
    validation::text,
};
use cairn_plugin_api::Manifest;
use cairn_plugin_host::{Denied, Host, InvocationError, Session};
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

/// Feeds one Gateway dispatch to the Discord package's transport worker, after READY
/// and the given threads under the channel, as Discord would. Returns whether a
/// person's message was queued for the package (access requests are not).
async fn gateway(
    s: &Service,
    connection: &Connection,
    threads: &[&str],
    name: &str,
    data: Value,
) -> cairn_installation::error::Result<bool> {
    let mut session = session(s, "discord").await;
    let (link, _commands) = socket::Link::channel();
    let mut frames = vec![(
        "READY",
        json!({ "session_id": "synthetic_session", "user": { "id": "400", "username": "Cairn", "bot": true } }),
    )];
    for thread in threads {
        frames.push((
            "THREAD_CREATE",
            json!({ "id": thread, "guild_id": "100", "parent_id": "200" }),
        ));
    }
    frames.push((name, data));
    let start = json!({ "type": "start", "settings": connection.settings });
    let result = async {
        socket::deliver(s, connection, &mut session, &link, start).await?;
        let personal = || async {
            let events = s.store.list(work::EVENTS).await?;
            cairn_installation::error::Result::Ok(
                events
                    .iter()
                    .filter(|event| event["authorId"].is_string())
                    .count(),
            )
        };
        let mut queued = false;
        for (sequence, (name, data)) in frames.into_iter().enumerate() {
            let before = personal().await?;
            let frame = json!({ "op": 0, "s": sequence + 1, "t": name, "d": data });
            let event = json!({ "type": "message", "text": frame.to_string() });
            socket::deliver(s, connection, &mut session, &link, event).await?;
            queued = personal().await? > before;
        }
        Ok(queued)
    }
    .await;
    session.stop().await;
    result
}

async fn accept(
    s: &Service,
    connection: &Connection,
    value: Value,
) -> cairn_installation::error::Result<bool> {
    gateway(s, connection, &[], "MESSAGE_CREATE", value).await
}

async fn accept_in(
    s: &Service,
    connection: &Connection,
    threads: &[&str],
    value: Value,
) -> cairn_installation::error::Result<bool> {
    gateway(s, connection, threads, "MESSAGE_CREATE", value).await
}

async fn interact(
    s: &Service,
    connection: &Connection,
    threads: &[&str],
    value: Value,
) -> cairn_installation::error::Result<bool> {
    gateway(s, connection, threads, "INTERACTION_CREATE", value).await
}

/// What a handler acts for: here, the Discord package handling `event`.
fn context(connection: &Connection, event: &Value) -> capabilities::Context {
    capabilities::Context {
        connection: connection.clone(),
        manifest: registry::builtin("discord").unwrap().manifest,
        event: event.clone(),
        link: None,
        draft: None,
        resource_grant: None,
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
    thread_status: AtomicUsize,
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
    if let Some(source) = path.strip_suffix("/threads") {
        let status = mock.thread_status.load(Ordering::SeqCst);
        if status != 0 {
            return StatusCode::from_u16(status as u16).unwrap().into_response();
        }
        // Like Discord, a thread started from a message reuses the message's ID.
        let id = source.rsplit('/').next().unwrap_or("700");
        return Json(json!({ "id": id })).into_response();
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
            "command",
            json!({
                "command": "read",
                "arguments": { "account": "a1", "reference": "op://vault/item/field" },
                "instances": [{ "id": "a1", "name": "Production" }],
            }),
            |method, input| async move {
                // The script names the secret; the host injects its value into `op`.
                assert_eq!(method, "exec.run");
                assert_eq!(input["program"], "op");
                assert_eq!(input["instance"], "a1");
                assert_eq!(
                    input["env"]["OP_SERVICE_ACCOUNT_TOKEN"],
                    json!({ "secret": "token" })
                );
                assert_eq!(
                    input["args"],
                    json!(["read", "--no-newline", "op://vault/item/field"])
                );
                Ok(json!({ "status": 0, "stdout": "synthetic_secret", "stderr": "" }))
            },
        )
        .await
        .unwrap();
    assert_eq!(value, "synthetic_secret");
    // A package's own message reaches people; other script errors never do.
    let error = host
        .invoke(
            &worker(),
            &onepassword.manifest,
            &onepassword.source,
            "command",
            json!({
                "command": "vaults",
                "arguments": { "account": "a2" },
                "instances": [{ "id": "a1", "name": "Production" }],
            }),
            |_, _| async { unreachable!() },
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        InvocationError::Rejected("This 1Password account is not authorized.".into())
    );

    let calls = AtomicUsize::new(0);
    let error = host
        .invoke(
            &worker(),
            &onepassword.manifest,
            "return { command = function(input) return cairn.call('native.execute', input) end }",
            "command",
            Value::Null,
            |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok(Value::Null) }
            },
        )
        .await
        .unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        matches!(error, InvocationError::Failed(ref message) if message.contains("capability"))
    );
    let error = host
        .invoke(
            &worker(),
            &onepassword.manifest,
            "return { command = function(input) error(input.secret) end }",
            "command",
            json!({ "secret": "synthetic_secret" }),
            |_, _| async { unreachable!() },
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        InvocationError::Failed("Plugin execution failed.".into())
    );
    let mut incompatible = onepassword.manifest.clone();
    incompatible.api_version += 1;
    assert!(
        host.start(&worker(), &incompatible, &onepassword.source)
            .await
            .is_err()
    );

    let manifest: Manifest = serde_json::from_value(json!({
        "id": "probe", "name": "Probe", "version": "1", "apiVersion": 2, "capabilities": ["storage"]
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
        matches!(error, InvocationError::Failed(ref message) if message.contains("deadline") || message.contains("failed")),
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
async fn rejected_handlers_keep_filtered_messages_and_leave_the_worker_usable() {
    let host = Host::default();
    let package = registry::builtin("discord").unwrap();
    let mut session = host
        .start(
            &worker(),
            &package.manifest,
            r#"return {
                reject = function(input) cairn.fail(input.message) end,
                fail = function() error("rejected: synthetic_private_detail") end,
                ok = function() return true end,
            }"#,
        )
        .await
        .unwrap();
    let message = format!("rejected: rejected: \n\t{}", "é".repeat(400));
    let expected: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect();
    assert_eq!(
        session
            .dispatch("reject", json!({ "message": message }), |_, _| async {
                unreachable!()
            })
            .await,
        Err(InvocationError::Rejected(expected))
    );
    assert!(session.alive());
    assert_eq!(
        session
            .dispatch("fail", Value::Null, |_, _| async { unreachable!() })
            .await,
        Err(InvocationError::Failed("Plugin execution failed.".into()))
    );
    assert!(session.alive());
    assert_eq!(
        session
            .dispatch("ok", Value::Null, |_, _| async { unreachable!() })
            .await
            .unwrap(),
        true
    );
    session.stop().await;
}

#[tokio::test]
async fn handler_presentations_preserve_rejection_and_host_refusal_policies() {
    let (_root, s, agent) = service().await;
    let mut manifest = registry::builtin("discord").unwrap().manifest;
    manifest.id = "presentations".into();
    manifest.instance_form = false;
    let (link, _commands) = socket::Link::channel();
    let rejected = "Review this instance.";
    let refused = "This host is not declared by the plugin.";
    let handler_failed = "The Discord plugin failed while handling test.";
    let socket_failed = "The plugin failed while handling its connection.";
    for (index, (body, handler_error, socket_error, start_message)) in [
        (
            "cairn.fail('Review this instance.')",
            (502, rejected),
            (403, rejected),
            "rejected: Review this instance.",
        ),
        (
            "error('rejected: synthetic_private_detail')",
            (502, handler_failed),
            (502, socket_failed),
            "Plugin execution failed.",
        ),
        (
            "local ok = pcall(cairn.http.request, { url = 'https://evil.example.org/' }) error('private')",
            (403, refused),
            (403, refused),
            "Plugin execution failed.",
        ),
        (
            "local ok = pcall(cairn.http.request, { url = 'https://evil.example.org/' }) cairn.fail('Review this instance.')",
            (502, rejected),
            (403, rejected),
            "rejected: Review this instance.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut manifest = manifest.clone();
        manifest.version = format!("99.0.0-presentation-{index}");
        let source = format!("return {{ test = function() {body} end, socket = function() {body} end, start = function() {body} end }}");
        let installed = registry::install(
            &s,
            &json!({
                "manifest": manifest,
                "source": source,
            }),
        )
        .await
        .unwrap();
        registry::approve(&s, &manifest.id, &manifest.version, text(&installed, "digest"))
            .await
            .unwrap();
        let package = registry::active(&s, &manifest.id).await.unwrap();
        let connection = connections::save(
            &s,
            &manifest.id,
            &json!({
                "name": "Presentation instance",
                "settings": {"guildId": "100", "channelId": "200", "botName": "Cairn"},
                "userIds": ["300"],
                "agentId": agent["id"],
                "enabled": true,
                "token": "synthetic_discord_token",
            }),
            None,
        )
        .await
        .unwrap();
        let connection: Connection = serde_json::from_value(connection).unwrap();
        let error = panels::test(&s, &manifest.id, &connection.id)
            .await
            .unwrap_err();
        assert_eq!((error.status, error.message.as_str()), handler_error);

        let mut session = s
            .plugins
            .host
            .start(&worker(), &package.manifest, &package.source)
            .await
            .unwrap();
        let error = socket::deliver(
            &s,
            &connection,
            &mut session,
            &link,
            json!({ "type": "start" }),
        )
        .await
        .unwrap_err();
        assert_eq!((error.status, error.message.as_str()), socket_error);
        assert!(session.alive());

        let error = runtime::begin(&s, &connection, &mut session)
            .await
            .unwrap_err();
        assert_eq!(
            error.status, 502,
            "Package start failures remain retryable."
        );
        assert_eq!(error.message, start_message);
        session.stop().await;
    }
}

/// Moves the delivery's next attempt and progress throttle to now.
async fn due(s: &Service) {
    for mut delivery in s.store.list(work::DELIVERIES).await.unwrap() {
        delivery["nextAttemptAt"] = json!(now());
        delivery["progressAt"] = json!(0);
        delivery["questionRetryAt"] = json!({});
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
        json!({ "channel": "500", "source": "200", "message": "500", "threaded": true })
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
    assert!(mock.called("POST", "/api/v10/channels/500/typing").await >= 1);
    let posts = mock.posts("/api/v10/channels/500/messages").await;
    let status = posts
        .iter()
        .find(|post| text(post, "content").starts_with('⏳'))
        .unwrap();
    assert_eq!(status["content"], "⏳ Running a command");
    assert_eq!(
        status["components"][0]["components"][0]["custom_id"],
        format!("stop:{chat}")
    );
    let asked = posts
        .iter()
        .find(|post| text(post, "content").contains("Retry the deploy?"))
        .unwrap();
    let buttons = &asked["components"][0]["components"];
    assert_eq!(buttons[1]["label"], "No");
    let custom = text(&buttons[1], "custom_id").to_owned();

    // A button press is acknowledged by the transport and answers the agent.
    let threads = ["500"];
    let click = json!({
        "id": "800", "token": "synthetic_interaction", "type": 3, "guild_id": "100", "channel_id": "500",
        "member": { "user": { "id": "300", "username": "sarah" } },
        "message": { "id": "9002", "author": { "id": "400" } },
        "data": { "custom_id": custom },
    });
    interact(&s, &connection, &threads, click.clone())
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
        mock.called("PATCH", "/api/v10/channels/500/messages/")
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

    let posts = mock.posts("/api/v10/channels/500/messages").await;
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
        mock.called("DELETE", "/api/v10/channels/500/messages/")
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
    follow["channel_id"] = json!("500");
    assert!(
        accept_in(&s, &connection, &threads, follow.clone())
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
    stop["channel_id"] = json!("500");
    accept_in(&s, &connection, &threads, stop.clone())
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(s.get("chats", &chat).await.unwrap()["paused"], true);
    assert_eq!(
        mock.called(
            "PUT",
            "/api/v10/channels/500/messages/502/reactions/%E2%8F%B9%EF%B8%8F/@me"
        )
        .await,
        1
    );
    session.stop().await;
    server.abort();
}

/// Completes the agent's turn for a delivery with `answer`, as the worker would.
async fn finish_turn(s: &Service, delivery: &Value, answer: &str) {
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
async fn public_support_channels_need_a_limited_agent_and_stay_quiet_when_nothing_is_needed() {
    let (_root, s, connection) = fixture().await;
    let (mock, server) = mock(&s).await;
    let support = s
        .agent(
            json!({
                "name": "Support",
                "access": { "projects": [], "skills": [], "mcps": [], "github": false, "installation": false, "sandbox": "read-only" },
            }),
            None,
        )
        .await
        .unwrap();
    let input = |agent: &Value, settings: Value| {
        json!({
            "name": "#support",
            "settings": settings,
            "accessMode": "everyone",
            "userIds": [],
            "agentId": agent["id"],
            "projectId": null,
            "maxMessagesPerHour": 20,
            "maxPerPersonPerHour": 2,
            "enabled": true,
        })
    };
    let channel = json!({ "guildId": "100", "channelId": "200", "messageContent": true, "everyMessage": true });
    // The fixture's agent can run commands: not acceptable for anyone in a channel.
    let broad = s.get("agents", &connection.agent_id).await.unwrap();
    assert!(
        connections::save(
            &s,
            "discord",
            &input(&broad, channel.clone()),
            Some(&connection.id)
        )
        .await
        .is_err()
    );
    // Answering every message needs Message Content: the package turns it on.
    let missing = json!({ "guildId": "100", "channelId": "200", "everyMessage": true });
    let saved = connections::save(
        &s,
        "discord",
        &input(&support, missing),
        Some(&connection.id),
    )
    .await
    .unwrap();
    assert_eq!(saved["settings"]["messageContent"], true);
    let saved = connections::save(
        &s,
        "discord",
        &input(&support, channel),
        Some(&connection.id),
    )
    .await
    .unwrap();
    let connection: Connection = serde_json::from_value(saved).unwrap();
    let mut session = session(&s, "discord").await;

    // A stranger's unmentioned message is a request, handled without any visible trace.
    let chatter = |id: &str, author: &str, content: &str| {
        let mut value = message(id, author, content);
        value["mentions"] = json!([]);
        value
    };
    assert!(
        accept(
            &s,
            &connection,
            chatter("600", "301", "thanks, that worked")
        )
        .await
        .unwrap()
    );
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let delivery = s.store.list(work::DELIVERIES).await.unwrap().remove(0);
    assert_eq!(delivery["reply"]["deferred"], true);
    let activity = s.store.list("plugin-activity").await.unwrap();
    assert!(text(&activity[0], "preview").starts_with("(Message from Sarah"));
    finish_turn(&s, &delivery, "NO_REPLY").await;
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(s.store.list(work::DELIVERIES).await.unwrap().is_empty());
    assert!(
        mock.requests
            .lock()
            .await
            .iter()
            .all(|call| call.method == "DELETE"),
        "no post, reaction, typing or thread"
    );

    // A real question gets its thread once the agent has an answer.
    assert!(
        accept(
            &s,
            &connection,
            chatter("601", "301", "How do I reset my password?")
        )
        .await
        .unwrap()
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let delivery = s.store.list(work::DELIVERIES).await.unwrap().remove(0);
    finish_turn(&s, &delivery, "Use the reset link on the sign-in page.").await;
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        mock.posts("/api/v10/channels/200/messages/601/threads")
            .await[0]["name"],
        "How do I reset my password?"
    );
    assert_eq!(
        mock.posts("/api/v10/channels/601/messages").await[0]["content"],
        "Use the reset link on the sign-in page."
    );
    assert_eq!(
        mock.called(
            "PUT",
            "/api/v10/channels/200/messages/601/reactions/%E2%9C%85/@me"
        )
        .await,
        1
    );

    // Each person has an hourly limit; the third message stays silent and starts nothing.
    assert!(
        accept(&s, &connection, chatter("602", "301", "one more thing"))
            .await
            .unwrap()
    );
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(s.store.list(work::DELIVERIES).await.unwrap().is_empty());
    assert!(
        mock.posts("/api/v10/channels/200/messages")
            .await
            .is_empty()
    );

    // Only whoever started a conversation can stop it.
    let threads = ["601"];
    let stop = |id: &str, author: &str| {
        let mut value = message(id, author, "<@400> stop");
        value["channel_id"] = json!("601");
        value
    };
    let binding = s.store.list(connections::KIND).await.unwrap();
    assert_eq!(binding[0]["accessMode"], "everyone");
    for (id, author) in [("603", "302"), ("604", "301")] {
        accept_in(&s, &connection, &threads, stop(id, author))
            .await
            .unwrap();
        runtime::pump(&s, &connection, &mut session).await.unwrap();
    }
    assert_eq!(
        mock.called(
            "PUT",
            "/api/v10/channels/601/messages/603/reactions/%E2%9D%8C/@me"
        )
        .await,
        1
    );
    assert_eq!(
        mock.called(
            "PUT",
            "/api/v10/channels/601/messages/604/reactions/%E2%8F%B9%EF%B8%8F/@me"
        )
        .await,
        1
    );

    // Granting the agent a plugin such as 1Password later suspends the public connection.
    connections::save(
        &s,
        "onepassword",
        &json!({
            "name": "Prod", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [support["id"]],
        }),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        accept(&s, &connection, chatter("605", "303", "hello?"))
            .await
            .unwrap_err()
            .status,
        403
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

#[tokio::test]
async fn revoked_and_deleted_deliveries_do_not_stop_other_people() {
    let (_root, s, connection) = fixture().await;
    let (mock, server) = mock(&s).await;
    let mut saved = serde_json::to_value(&connection).unwrap();
    saved["userIds"] = json!(["300", "301"]);
    saved["maxMessagesPerHour"] = json!(10);
    let connection: Connection = serde_json::from_value(
        connections::save(&s, "discord", &saved, Some(&connection.id))
            .await
            .unwrap(),
    )
    .unwrap();
    let mut session = session(&s, "discord").await;
    for (id, author) in [("510", "300"), ("511", "301"), ("512", "301")] {
        accept(&s, &connection, message(id, author, "<@400> hello"))
            .await
            .unwrap();
    }
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let deliveries = s.store.list(work::DELIVERIES).await.unwrap();
    let removed = deliveries.iter().find(|d| d["thread"] == "511").unwrap();
    let chat_id = text(removed, "chatId").to_owned();
    s.store
        .transaction(move |db| db.remove("chats", &chat_id))
        .await
        .unwrap();
    connections::people(&s, "discord", &connection.id, "300", "remove")
        .await
        .unwrap();
    let remaining = deliveries.iter().find(|d| d["thread"] == "512").unwrap();
    finish_turn(&s, remaining, "Still here").await;
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert!(session.alive());
    assert!(s.store.list(work::DELIVERIES).await.unwrap().is_empty());
    assert_eq!(
        mock.posts("/api/v10/channels/512/messages")
            .await
            .last()
            .unwrap()["content"],
        "Still here"
    );
    assert!(
        mock.posts("/api/v10/channels/510/messages")
            .await
            .is_empty()
    );
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn public_question_partial_answers_require_the_starters_authority() {
    let (_root, s, connection) = fixture().await;
    let agent = s.agent(json!({
        "name": "Support",
        "access": { "projects": [], "skills": [], "mcps": [], "github": false, "installation": false, "sandbox": "read-only" },
    }), None).await.unwrap();
    let mut saved = serde_json::to_value(&connection).unwrap();
    saved["agentId"] = agent["id"].clone();
    saved["accessMode"] = json!("everyone");
    let connection: Connection = serde_json::from_value(
        connections::save(&s, "discord", &saved, Some(&connection.id))
            .await
            .unwrap(),
    )
    .unwrap();
    let (_mock, server) = mock(&s).await;
    let mut session = session(&s, "discord").await;
    accept(&s, &connection, message("520", "301", "<@400> help"))
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let delivery = s.store.list(work::DELIVERIES).await.unwrap().remove(0);
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.get("chats", text(&delivery, "chatId")).await.unwrap();
    let run = text(&chat, "runId");
    s.store
        .event(
            run,
            "chat.user",
            "",
            Some(json!({ "messageId": delivery["id"] })),
        )
        .await
        .unwrap();
    let question = "b".repeat(64);
    s.question_receive(run, json!({
        "id": question,
        "blocking": true,
        "fields": [
            { "id": "first", "title": "First?", "secret": false, "options": [{ "label": "Safe", "description": "" }, { "label": "Attacker", "description": "" }] },
            { "id": "second", "title": "Second?", "secret": false, "options": [] },
        ],
    })).await.unwrap();
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let key = format!(
        "storage:{}:{}",
        connection.id,
        cairn_installation::auth::hex_digest(&format!("question:520:{question}"))
    );
    let threads = ["520"];
    // A stranger's button cannot poison the first field.
    let click = json!({
        "id": "801", "token": "synthetic_interaction", "type": 3, "guild_id": "100", "channel_id": "520",
        "member": { "user": { "id": "302", "username": "outsider" } },
        "message": { "id": "9002", "author": { "id": "400" } },
        "data": { "custom_id": format!("a:{question}:0:1") },
    });
    interact(&s, &connection, &threads, click.clone())
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        s.store.get("plugin-storage", &key).await.unwrap().unwrap()["value"]["answers"],
        json!({})
    );
    // A stranger's free text cannot poison it either; the starter can answer both.
    for (id, author, answer) in [
        ("521", "302", "Attacker"),
        ("522", "301", "Safe"),
        ("523", "301", "Continue"),
    ] {
        let mut value = message(id, author, answer);
        value["channel_id"] = json!("520");
        accept_in(&s, &connection, &threads, value.clone())
            .await
            .unwrap();
        runtime::pump(&s, &connection, &mut session).await.unwrap();
    }
    let queued = s
        .store
        .read({
            let chat_id = text(&chat, "id").to_owned();
            move |db| db.messages(&chat_id)
        })
        .await
        .unwrap();
    assert!(
        queued
            .iter()
            .any(|message| message["answers"]
                == json!({ "first": ["Safe"], "second": ["Continue"] }))
    );
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn failures_before_acknowledgement_finish_and_followups_resume_stopped_runs() {
    for status in ["cancelled", "failed", "interrupted"] {
        let (_root, s, connection) = fixture().await;
        let event = json!({ "name": "message", "revision": connection.revision, "authorId": "300", "payload": { "id": "530" } });
        let args = json!({ "text": "hello", "thread": "530", "shared": true });
        let receipt = work::submit(&s, &context(&connection, &event), &args)
            .await
            .unwrap();
        s.chat_tick(&std::collections::HashSet::new())
            .await
            .unwrap();
        let chat = s.get("chats", text(&receipt, "chatId")).await.unwrap();
        let run = text(&chat, "runId");
        s.store
            .patch_run(run, json!({ "status": status, "recoveryPending": false }))
            .await
            .unwrap();
        let delivery = s.store.list(work::DELIVERIES).await.unwrap().remove(0);
        let reply = work::reply(&s, &connection, &delivery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            reply.status,
            if status == "cancelled" {
                "cancelled"
            } else {
                "failed"
            }
        );
        if status != "interrupted" {
            s.chat_tick(&std::collections::HashSet::new())
                .await
                .unwrap();
            assert_eq!(
                s.get("chats", text(&chat, "id")).await.unwrap()["paused"],
                true
            );
            let mut follow = event.clone();
            follow["payload"]["id"] = json!("531");
            work::submit(&s, &context(&connection, &follow), &args)
                .await
                .unwrap();
            s.chat_tick(&std::collections::HashSet::new())
                .await
                .unwrap();
            assert_eq!(s.store.run(run).await.unwrap()["status"], "queued");
            assert_eq!(
                s.get("chats", text(&chat, "id")).await.unwrap()["paused"],
                false
            );
        }
    }
}

#[tokio::test]
async fn channel_fallback_questions_retry_and_controls_route_to_the_right_conversation() {
    let (_root, s, connection) = fixture().await;
    let (mock, server) = mock(&s).await;
    mock.thread_status.store(403, Ordering::SeqCst);
    let mut session = session(&s, "discord").await;
    accept(&s, &connection, message("540", "300", "<@400> help"))
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let delivery = s.store.list(work::DELIVERIES).await.unwrap().remove(0);
    assert_eq!(delivery["reply"]["threaded"], false);
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.get("chats", text(&delivery, "chatId")).await.unwrap();
    let run = text(&chat, "runId");
    s.store
        .event(
            run,
            "chat.user",
            "",
            Some(json!({ "messageId": delivery["id"] })),
        )
        .await
        .unwrap();
    let question = "c".repeat(64);
    s.question_receive(run, json!({
        "id": question, "blocking": true,
        "fields": [{ "id": "x".repeat(101), "title": "Choose?", "secret": false, "options": [{ "label": format!("a{}", "é".repeat(40)), "description": "" }] }],
    })).await.unwrap();
    // Another queued message in this conversation must not republish its question.
    let mut second = message("541", "300", "<@400> also this");
    second["attachments"] = json!([]);
    accept(&s, &connection, second).await.unwrap();
    mock.statuses.lock().await.push(429);
    due(&s).await;
    for mut delivery in s.store.list(work::DELIVERIES).await.unwrap() {
        delivery["progressAt"] = json!(now());
        s.store.put(work::DELIVERIES, delivery).await.unwrap();
    }
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let first = s
        .store
        .get(work::DELIVERIES, text(&delivery, "id"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first["asked"], json!([]));
    due(&s).await;
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let asked = mock
        .posts("/api/v10/channels/200/messages")
        .await
        .into_iter()
        .filter(|post| text(post, "content").contains("Choose?"))
        .collect::<Vec<_>>();
    assert_eq!(
        asked.len(),
        2,
        "one failed post and one successful retry; queued delivery posts none"
    );
    assert_eq!(
        asked[1]["components"][0]["components"][0]["label"],
        format!("a{}", "é".repeat(40))
    );
    accept(
        &s,
        &connection,
        message("542", "300", &format!("<@400> {}", "界".repeat(1800))),
    )
    .await
    .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let pending = s
        .store
        .kv(&format!("chat-question:{}:{question}", text(&chat, "id")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending["status"], "answering");
    // Stop buttons include the explicit conversation even without a shared thread.
    interact(&s, &connection, &[], json!({
        "id": "802", "token": "synthetic_interaction", "type": 3, "guild_id": "100", "channel_id": "200",
        "member": { "user": { "id": "300", "username": "sarah" } },
        "message": { "id": "9002", "author": { "id": "400" } },
        "data": { "custom_id": format!("stop:{}", text(&chat, "id")) },
    })).await.unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    assert_eq!(
        s.get("chats", text(&chat, "id")).await.unwrap()["paused"],
        true
    );
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn every_host_capability_rejects_a_changed_owner() {
    let (_root, s, mut connection) = fixture().await;
    // The persisted connection keeps valid agent grants but belongs to another owner.
    connection.owner_access_id = Some("previous-owner".into());
    s.store
        .put(
            connections::KIND,
            serde_json::to_value(&connection).unwrap(),
        )
        .await
        .unwrap();
    let ctx = context(
        &connection,
        &json!({ "name": "timer", "revision": connection.revision }),
    );
    for (method, args) in [
        ("storage.set", json!({ "key": "x", "value": true })),
        ("timers.after", json!({ "seconds": 1, "name": "x" })),
        (
            "http.request",
            json!({ "url": "https://discord.com/api/v10/users/@me", "auth": false }),
        ),
    ] {
        let denied = capabilities::call(&s, &ctx, method, args)
            .await
            .err()
            .unwrap();
        assert_eq!(denied.0, "forbidden");
    }
    assert!(s.store.list("plugin-storage").await.unwrap().is_empty());
    assert!(s.store.list("plugin-timers").await.unwrap().is_empty());
}

#[tokio::test]
async fn optional_questions_are_not_republished_on_later_turns() {
    let (_root, s, connection) = fixture().await;
    let (mock, server) = mock(&s).await;
    let mut session = session(&s, "discord").await;
    accept(&s, &connection, message("550", "300", "<@400> help"))
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let first = s.store.list(work::DELIVERIES).await.unwrap().remove(0);
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.get("chats", text(&first, "chatId")).await.unwrap();
    let run = text(&chat, "runId");
    s.store
        .event(
            run,
            "chat.user",
            "",
            Some(json!({ "messageId": first["id"] })),
        )
        .await
        .unwrap();
    let question = "d".repeat(64);
    s.question_receive(run, json!({ "id": question, "blocking": false, "fields": [{ "id": "0", "title": "Optional?", "secret": false, "options": [] }] })).await.unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    s.question_release(run, Some(&question)).await.unwrap();
    s.store
        .event(
            run,
            "item.completed",
            "Done",
            Some(json!({ "item": { "type": "agent_message", "text": "Done" } })),
        )
        .await
        .unwrap();
    s.store
        .event(run, "turn.completed", "", None)
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let event = json!({ "name": "message", "revision": connection.revision, "authorId": "300", "payload": { "id": "551" } });
    let receipt = work::submit(&s, &context(&connection, &event), &json!({ "text": "Another thing", "thread": "550", "shared": true, "reply": first["reply"] })).await.unwrap();
    s.store
        .patch_run(
            run,
            json!({ "chatExecution": { "messageId": receipt["messageId"] } }),
        )
        .await
        .unwrap();
    s.store
        .event(
            run,
            "chat.user",
            "",
            Some(json!({ "messageId": receipt["messageId"] })),
        )
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    let posts = mock.posts("/api/v10/channels/550/messages").await;
    assert_eq!(
        posts
            .iter()
            .filter(|post| text(post, "content").contains("Optional?"))
            .count(),
        1
    );
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn reviewed_reduced_agent_grants_start_a_fresh_conversation() {
    let (_root, s, connection) = fixture().await;
    let event = |id| json!({ "name": "message", "revision": connection.revision, "authorId": "300", "payload": { "id": id } });
    let args = json!({ "text": "hello", "thread": "560", "shared": true });
    let first = work::submit(&s, &context(&connection, &event("560")), &args)
        .await
        .unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let mut agent = s.get("agents", &connection.agent_id).await.unwrap();
    agent["access"]["sandbox"] = json!("read-only");
    s.agent(agent, Some(&connection.agent_id)).await.unwrap();
    let saved = connections::save(
        &s,
        "discord",
        &serde_json::to_value(&connection).unwrap(),
        Some(&connection.id),
    )
    .await
    .unwrap();
    let new_connection: Connection = serde_json::from_value(saved).unwrap();
    let mut next = event("561");
    next["revision"] = json!(new_connection.revision);
    let second = work::submit(&s, &context(&new_connection, &next), &args)
        .await
        .unwrap();
    assert_ne!(first["chatId"], second["chatId"]);
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.get("chats", text(&second, "chatId")).await.unwrap();
    let run = s.store.run(text(&chat, "runId")).await.unwrap();
    assert_eq!(run["snapshot"]["agent"]["access"]["sandbox"], "read-only");
}

#[tokio::test]
async fn concurrent_plugin_uploads_share_the_chat_attachment_quota() {
    let (_root, s, connection) = fixture().await;
    let chat = s
        .chat_create(json!({ "agentId": connection.agent_id, "projectId": null }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id");
    let seed = cairn_installation::config::id();
    s.store
        .set(
            &format!("chat-attachment:{chat_id}:{seed}"),
            json!({ "id": seed, "size": 200 * 1024 * 1024 - 3 }),
            None,
        )
        .await
        .unwrap();
    let first = cairn_installation::config::id();
    let second = cairn_installation::config::id();
    let (left, right) = tokio::join!(
        s.store_attachment(chat_id, &first, "one.txt", b"12"),
        s.store_attachment(chat_id, &second, "two.txt", b"34"),
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    assert_eq!(left.err().or_else(|| right.err()).unwrap().status, 413);
}

fn pinger(version: &str, capabilities: &[&str]) -> Value {
    json!({
        "manifest": {
            "id": "pinger",
            "name": "Pinger",
            "version": version,
            "description": "Pings a status API.",
            "apiVersion": 2,
            "capabilities": capabilities,
            "hosts": ["api.example.com"],
            "credential": { "label": "API key", "header": "x-api-key" },
            "settings": [{ "key": "team", "title": "Team", "type": "string", "required": true }],
        },
        "source": r#"return {
            start = function(event)
                cairn.timers.after(60, "ping", { team = event.settings.team })
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
        json!({ "manifest": { "id": "discord", "name": "Fake", "version": "1", "apiVersion": 2, "capabilities": [] }, "source": "return {}" }),
        json!({ "manifest": { "id": "broken", "name": "Broken", "version": "1", "apiVersion": 2, "capabilities": [] }, "source": "return function() end" }),
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
    let input = json!({ "name": "Core status", "settings": { "team": "configured-team" }, "token": "secret-key", "enabled": true });
    assert_eq!(
        connections::save(&s, "pinger", &input, None)
            .await
            .unwrap_err()
            .status,
        403
    );

    let source_path = root.path().join("data/plugins/pinger/1.0.0/main.luau");
    let original_source = text(&package, "source");
    std::fs::write(
        &source_path,
        format!("{original_source}\n-- changed after review"),
    )
    .unwrap();
    assert_eq!(
        registry::approve(&s, "pinger", "1.0.0", text(&entry["versions"][0], "digest"))
            .await
            .unwrap_err()
            .status,
        409
    );
    assert!(registry::active(&s, "pinger").await.is_err());
    std::fs::write(&source_path, original_source).unwrap();
    registry::approve(&s, "pinger", "1.0.0", text(&entry["versions"][0], "digest"))
        .await
        .unwrap();
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
    assert_eq!(requests[0].body, json!({ "team": "configured-team" }));
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
    registry::approve(
        &s,
        "pinger",
        "1.1.0",
        text(
            entry["versions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["version"] == "1.1.0")
                .unwrap(),
            "digest",
        ),
    )
    .await
    .unwrap();
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

#[tokio::test]
async fn installed_packages_can_be_split_into_modules_and_any_file_change_needs_review() {
    let (root, s, _) = service().await;
    let package = |modules: Value| {
        json!({
            "manifest": {
                "id": "splitter", "name": "Splitter", "version": "1.0.0", "apiVersion": 2,
                "capabilities": [],
            },
            "source": "local greet = require('./lib/greet')\nreturn { probe = function(event) return greet.hello(event.name) end }",
            "modules": modules,
        })
    };
    for invalid in [
        json!({ "../escape.luau": "return {}" }),
        json!({ "lib/Greet.luau": "return {}" }),
        json!({ "lib/greet.luau": 42 }),
        json!({ "lib/other.luau": "return {}" }),
        json!({ "lib/greet.luau": "return require('./missing')" }),
    ] {
        assert!(
            registry::install(&s, &package(invalid.clone()))
                .await
                .is_err(),
            "{invalid}"
        );
    }
    let modules = json!({
        "lib/greet.luau": "local text = require('./text')\nreturn { hello = function(name) return text.upper('hello ') .. name end }",
        "lib/text.luau": "return { upper = string.upper }",
    });
    let installed = registry::install(&s, &package(modules)).await.unwrap();
    assert_eq!(
        installed["files"],
        json!([
            { "path": "main.luau", "bytes": 106 },
            { "path": "lib/greet.luau", "bytes": 104 },
            { "path": "lib/text.luau", "bytes": 31 },
        ])
    );
    let directory = root.path().join("data/plugins/splitter/1.0.0");
    assert!(directory.join("lib/greet.luau").is_file());
    registry::approve(&s, "splitter", "1.0.0", text(&installed, "digest"))
        .await
        .unwrap();
    let active = registry::active(&s, "splitter").await.unwrap();
    assert_eq!(active.source.modules.len(), 2);
    let result = s
        .plugins
        .host
        .invoke(
            &worker(),
            &active.manifest,
            &active.source,
            "probe",
            json!({ "name": "Ada" }),
            |_, _| async { Err(Denied("forbidden")) },
        )
        .await
        .unwrap();
    assert_eq!(result, "HELLO Ada");

    // Editing a module, adding one or linking one withdraws the approval.
    let original = std::fs::read_to_string(directory.join("lib/text.luau")).unwrap();
    std::fs::write(
        directory.join("lib/text.luau"),
        "return { upper = string.lower }",
    )
    .unwrap();
    assert_eq!(
        registry::active(&s, "splitter").await.err().unwrap().status,
        403
    );
    std::fs::write(directory.join("lib/text.luau"), &original).unwrap();
    registry::active(&s, "splitter").await.unwrap();
    std::fs::write(directory.join("extra.luau"), "return {}").unwrap();
    assert_eq!(
        registry::active(&s, "splitter").await.err().unwrap().status,
        403
    );
    std::fs::remove_file(directory.join("extra.luau")).unwrap();
    std::os::unix::fs::symlink(directory.join("main.luau"), directory.join("link.luau")).unwrap();
    assert!(registry::active(&s, "splitter").await.is_err());
    std::fs::remove_file(directory.join("link.luau")).unwrap();
    registry::active(&s, "splitter").await.unwrap();
}
