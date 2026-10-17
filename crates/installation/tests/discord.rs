//! The Discord package runs entirely in Luau: its Gateway session over the generic
//! `socket` capability, its setup form, its test and its panel. Discord is simulated
//! by local HTTP and WebSocket servers.
mod common;

use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use cairn_installation::{
    plugins::{
        connections::{self, Connection},
        forms, panels, registry, socket, work,
    },
    service::Service,
    validation::text,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

const VIEW: u64 = 1 << 10;
const SEND: u64 = 1 << 11;
const HISTORY: u64 = 1 << 16;

async fn service() -> (TempDir, Arc<Service>, Value) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
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

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .is_some_and(|value| value == "Bot synthetic_discord_token")
}

fn guild() -> Value {
    json!({
        "id": "100",
        "owner_id": "1",
        "roles": [
            { "id": "100", "permissions": (VIEW | SEND | HISTORY).to_string() },
            { "id": "7", "permissions": "0" },
        ],
    })
}

/// What GET /gateway/bot answers, and how many times it was asked.
#[derive(Clone)]
struct GatewayBot {
    info: Arc<std::sync::Mutex<Value>>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl GatewayBot {
    fn remaining(&self, remaining: u64, reset_after: u64) {
        *self.info.lock().unwrap() = json!({
            "url": "wss://gateway-test.discord.gg",
            "shards": 1,
            "session_start_limit": { "total": 1000, "remaining": remaining, "reset_after": reset_after, "max_concurrency": 1 },
        });
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Discord's REST API, as much as setup, the Gateway and the tests use.
async fn rest() -> (String, tokio::task::JoinHandle<()>) {
    let (origin, server, _) = rest_with_gateway().await;
    (origin, server)
}

async fn rest_with_gateway() -> (String, tokio::task::JoinHandle<()>, GatewayBot) {
    async fn me(headers: HeaderMap) -> impl IntoResponse {
        if !authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, Json(json!({ "code": 0 })));
        }
        (
            StatusCode::OK,
            Json(json!({ "id": "400", "username": "cairn", "bot": true })),
        )
    }
    let gateway = GatewayBot {
        info: Arc::default(),
        calls: Arc::default(),
    };
    gateway.remaining(999, 0);
    let app = Router::new()
        .route("/api/v10/users/@me", get(me))
        .route(
            "/api/v10/oauth2/applications/@me",
            get(|| async { Json(json!({ "id": "401" })) }),
        )
        .route(
            "/api/v10/users/@me/guilds",
            get(|| async { Json(json!([{ "id": "100", "name": "Acme" }])) }),
        )
        .route(
            "/api/v10/guilds/{id}",
            get(|Path(id): Path<String>| async move {
                if id == "100" {
                    Json(guild()).into_response()
                } else {
                    StatusCode::NOT_FOUND.into_response()
                }
            }),
        )
        .route(
            "/api/v10/guilds/100/members/400",
            get(|| async { Json(json!({ "roles": ["7"] })) }),
        )
        .route(
            "/api/v10/guilds/100/channels",
            get(|| async {
                Json(json!([
                    { "id": "201", "type": 0, "name": "private", "position": 2, "permission_overwrites": [
                        { "id": "100", "type": 0, "allow": "0", "deny": VIEW.to_string() }
                    ] },
                    { "id": "200", "type": 0, "name": "support", "position": 1, "permission_overwrites": [] },
                    { "id": "203", "type": 0, "name": "staff", "position": 3, "permission_overwrites": [
                        { "id": "100", "type": 0, "allow": "0", "deny": VIEW.to_string() },
                        { "id": "7", "type": 0, "allow": VIEW.to_string(), "deny": "0" }
                    ] },
                    { "id": "204", "type": 0, "name": "quiet", "position": 4, "permission_overwrites": [
                        { "id": "400", "type": 1, "allow": "0", "deny": SEND.to_string() }
                    ] },
                    { "id": "202", "type": 2, "name": "voice", "position": 0 }
                ]))
            }),
        )
        .route(
            "/api/v10/gateway/bot",
            get({
                let gateway = gateway.clone();
                move |headers: HeaderMap| async move {
                    assert!(authorized(&headers));
                    gateway
                        .calls
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Json(gateway.info.lock().unwrap().clone())
                }
            }),
        )
        .route(
            "/api/v10/guilds/100/threads/active",
            get(|| async {
                Json(json!({ "threads": [
                    { "id": "700", "guild_id": "100", "parent_id": "200", "thread_metadata": { "archived": false } },
                    { "id": "701", "guild_id": "100", "parent_id": "999" },
                ], "members": [] }))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin, server, gateway)
}

async fn save(
    s: &Service,
    agent: &Value,
    settings: Value,
) -> cairn_installation::error::Result<Value> {
    connections::save(
        s,
        "discord",
        &json!({
            "name": "#support on Acme",
            "settings": settings,
            "userIds": ["300"],
            "agentId": agent["id"],
            "projectId": null,
            "enabled": true,
            "token": "synthetic_discord_token",
        }),
        None,
    )
    .await
}

#[tokio::test]
async fn setup_form_lists_servers_and_channels_the_bot_can_use() {
    let (_root, s, agent) = service().await;
    let (origin, server) = rest().await;
    s.plugins.route_http_to(Some(origin));
    assert!(registry::builtin("discord").unwrap().manifest.instance_form);

    // Before a token: guidance only.
    let empty = forms::form(&s, "discord", &json!({ "settings": {} }))
        .await
        .unwrap();
    assert_eq!(empty["view"][0]["title"], "Create the bot");
    assert_eq!(empty["options"], json!({}));

    let result = forms::form(
        &s,
        "discord",
        &json!({ "token": "synthetic_discord_token", "settings": { "guildId": "100", "channelId": "200" } }),
    )
    .await
    .unwrap();
    assert_eq!(result["errors"], json!({}), "{result}");
    assert_eq!(
        result["options"]["guildId"],
        json!([{ "value": "100", "label": "Acme" }])
    );
    let channels = result["options"]["channelId"].as_array().unwrap();
    let names = channels
        .iter()
        .map(|channel| text(channel, "label"))
        .collect::<Vec<_>>();
    assert_eq!(names, ["#support", "#private", "#staff", "#quiet"]);
    let channel = |name: &str| {
        channels
            .iter()
            .find(|channel| channel["label"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(channel("#support")["disabled"], false);
    assert_eq!(
        channel("#support")["description"],
        "No threads · replies in the channel"
    );
    assert_eq!(channel("#private")["disabled"], true);
    assert_eq!(
        channel("#private")["description"],
        "The bot can’t read this channel"
    );
    assert_eq!(
        channel("#staff")["disabled"],
        false,
        "role overwrite allows"
    );
    assert_eq!(
        channel("#quiet")["description"],
        "The bot can’t send messages here"
    );
    assert_eq!(
        result["values"],
        json!({ "botId": "400", "botName": "cairn", "guildName": "Acme", "channelName": "support" })
    );
    assert_eq!(result["view"][0]["title"], "Signed in as cairn");
    assert_eq!(
        result["view"][1]["url"],
        "https://discord.com/oauth2/authorize?client_id=401&scope=bot&permissions=309237746752"
    );

    // A new server resets the channel; unknown servers and bad tokens explain themselves.
    let changed = forms::form(
        &s,
        "discord",
        &json!({ "token": "synthetic_discord_token", "settings": { "guildId": "100", "channelId": "200" }, "changed": "guildId" }),
    )
    .await
    .unwrap();
    assert_eq!(changed["values"]["channelId"], "");
    let unknown = forms::form(
        &s,
        "discord",
        &json!({ "token": "synthetic_discord_token", "settings": { "guildId": "999" } }),
    )
    .await
    .unwrap();
    assert_eq!(
        unknown["errors"]["guildId"],
        "The bot isn’t in this server yet. Invite it, then refresh."
    );
    let rejected = forms::form(
        &s,
        "discord",
        &json!({ "token": "wrong_token", "settings": {} }),
    )
    .await
    .unwrap();
    assert!(
        text(&rejected["errors"], "credential").starts_with("Discord rejected this token"),
        "{rejected}"
    );

    // Saving checks the IDs and turns on Message Content for support channels.
    for settings in [
        json!({ "guildId": "100" }),
        json!({ "guildId": "100", "channelId": "x" }),
        json!({ "guildId": "100", "channelId": "200", "extra": 1 }),
    ] {
        assert!(save(&s, &agent, settings).await.is_err());
    }
    let saved = save(
        &s,
        &agent,
        json!({ "guildId": "100", "channelId": "200", "everyMessage": true, "channelName": "support" }),
    )
    .await
    .unwrap();
    assert_eq!(saved["settings"]["messageContent"], true);
    let connection: Connection = serde_json::from_value(saved).unwrap();

    // Editing reuses the saved token; the test and the panel run in Luau too.
    let editing = forms::form(
        &s,
        "discord",
        &json!({ "instance": connection.id, "settings": connection.settings }),
    )
    .await
    .unwrap();
    assert_eq!(editing["values"]["botName"], "cairn");
    let tested = panels::test(&s, "discord", &connection.id).await.unwrap();
    assert_eq!(tested["message"], "Token works · signed in as cairn");
    let panel = panels::view(&s, "discord", &connection.id).await.unwrap();
    assert_eq!(panel["view"][0]["items"][2]["value"], "#support");
    assert_eq!(
        panel["view"][0]["items"][3]["value"],
        "Every message in the channel"
    );
    server.abort();
}

async fn send(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    value: Value,
) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn receive(socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Value {
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            Message::Close(_) => panic!("closed"),
            _ => {}
        }
    }
}

fn mention(sequence: u64) -> Value {
    json!({
        "op": 0, "s": sequence, "t": "MESSAGE_CREATE",
        "d": {
            "id": "500", "guild_id": "100", "channel_id": "200", "content": "<@400> hello",
            "author": { "id": "300", "username": "sarah" },
            "mentions": [{ "id": "400" }],
        },
    })
}

#[tokio::test]
async fn gateway_identifies_heartbeats_admits_mentions_and_resumes_in_luau() {
    let (_root, s, agent) = service().await;
    let saved = save(&s, &agent, json!({ "guildId": "100", "channelId": "200" }))
        .await
        .unwrap();
    let connection: Connection = serde_json::from_value(saved).unwrap();
    let (origin, server, gateway) = rest_with_gateway().await;
    s.plugins.route_http_to(Some(origin));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    s.plugins
        .route_sockets_to(Some(format!("ws://{}", listener.local_addr().unwrap())));
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        send(
            &mut socket,
            json!({ "op": 10, "d": { "heartbeat_interval": 300 } }),
        )
        .await;
        let identify = receive(&mut socket).await;
        assert_eq!(identify["op"], 2);
        assert_eq!(identify["d"]["intents"], 513);
        // The host put the token where the script wrote {{secret}}.
        assert_eq!(identify["d"]["token"], "synthetic_discord_token");
        send(
            &mut socket,
            json!({ "op": 0, "s": 1, "t": "READY", "d": {
                "session_id": "synthetic_session",
                "user": { "id": "400", "username": "Cairn", "bot": true },
                "resume_gateway_url": "wss://gateway-us-east1-b.discord.gg",
            } }),
        )
        .await;
        // A replayed dispatch must not create a second inbox record.
        send(&mut socket, mention(2)).await;
        send(&mut socket, mention(3)).await;
        // Heartbeats carry the last sequence once the dispatches are handled.
        loop {
            let heartbeat = receive(&mut socket).await;
            assert_eq!(heartbeat["op"], 1);
            send(&mut socket, json!({ "op": 11 })).await;
            if heartbeat["d"] == 3 {
                break;
            }
        }
        send(&mut socket, json!({ "op": 7, "d": null })).await;

        // Reconnect requested: the script resumes the same session.
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        send(
            &mut socket,
            json!({ "op": 10, "d": { "heartbeat_interval": 300 } }),
        )
        .await;
        let resume = receive(&mut socket).await;
        assert_eq!(resume["op"], 6);
        assert_eq!(
            resume["d"],
            json!({ "token": "synthetic_discord_token", "session_id": "synthetic_session", "seq": 3 })
        );
        send(
            &mut socket,
            json!({ "op": 0, "s": 4, "t": "RESUMED", "d": {} }),
        )
        .await;
        // An invalid token closes the session for good.
        socket
            .close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: 4004.into(),
                reason: "Authentication failed.".into(),
            }))
            .await
            .unwrap();
    });
    let package = registry::active(&s, "discord").await.unwrap();
    let stop = CancellationToken::new();
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        socket::run(&s, &connection, &package, &stop),
    )
    .await
    .unwrap();
    upstream.await.unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.status, 403);
    assert!(
        error
            .message
            .starts_with("Discord rejected the bot configuration")
    );
    // A new session asks Discord for its URL and session budget; a resume does not.
    assert_eq!(gateway.calls(), 1);
    server.abort();
    let events = s.store.list(work::EVENTS).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"]["content"], "<@400> hello");
    assert_eq!(events[0]["payload"]["botId"], "400");
    let status = s
        .store
        .kv(&format!("plugin-status:{}", connection.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status["state"], "connected");
    assert_eq!(status["message"], "Signed in as Cairn");
}

#[tokio::test]
async fn sockets_reach_only_declared_hosts_from_the_transport_worker() {
    let (_root, s, agent) = service().await;
    let saved = save(&s, &agent, json!({ "guildId": "100", "channelId": "200" }))
        .await
        .unwrap();
    let connection: Connection = serde_json::from_value(saved).unwrap();
    let (origin, server, _) = rest_with_gateway().await;
    s.plugins.route_http_to(Some(origin));
    let package = registry::active(&s, "discord").await.unwrap();
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
    let (link, mut commands) = socket::Link::channel();
    for event in [
        json!({ "type": "start", "settings": connection.settings }),
        json!({ "type": "tick", "name": "connect" }),
    ] {
        socket::deliver(&s, &connection, &mut session, &link, event)
            .await
            .unwrap();
    }
    assert!(matches!(
        commands.try_recv().unwrap(),
        socket::Command::Timer { .. }
    ));
    match commands.try_recv().unwrap() {
        socket::Command::Connect { url, header, .. } => {
            assert_eq!(url, "wss://gateway-test.discord.gg/?v=10&encoding=json");
            assert!(header.is_none());
        }
        _ => panic!("expected a connection"),
    }
    let context = cairn_installation::plugins::capabilities::Context {
        connection: connection.clone(),
        manifest: package.manifest.clone(),
        event: json!({ "name": "socket" }),
        link: Some(link.clone()),
        draft: None,
    };
    for url in [
        "wss://gateway.discord.gg.attacker.example",
        "ws://gateway.discord.gg",
        "wss://discord.gg",
        "wss://user@gateway.discord.gg",
    ] {
        let refused = cairn_installation::plugins::capabilities::checked(
            &s,
            &context,
            "socket.connect",
            json!({ "url": url }),
        )
        .await;
        assert!(refused.is_err(), "{url}");
    }
    // Paced sends with one credential wait for each other, as Identify must.
    let started = std::time::Instant::now();
    for _ in 0..2 {
        cairn_installation::plugins::capabilities::checked(
            &s,
            &context,
            "socket.send",
            json!({ "text": "{\"token\":\"{{secret}}\"}", "auth": true, "pace": 400 }),
        )
        .await
        .unwrap();
    }
    assert!(started.elapsed() >= Duration::from_millis(350));
    // Without a transport worker there is no socket, and secrets need their marker.
    let mut detached = context;
    let refused = cairn_installation::plugins::capabilities::checked(
        &s,
        &detached,
        "socket.send",
        json!({ "text": "{\"token\":\"x\"}", "auth": true }),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.status, 400);
    detached.link = None;
    let refused = cairn_installation::plugins::capabilities::checked(
        &s,
        &detached,
        "socket.send",
        json!({ "text": "hello" }),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.status, 403);
    session.stop().await;
    server.abort();
}

/// The transport worker's commands for one `socket` event, in order.
async fn handle(
    s: &Service,
    connection: &Connection,
    session: &mut cairn_plugin_host::Session,
    link: &socket::Link,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<socket::Command>,
    event: Value,
) -> Vec<socket::Command> {
    socket::deliver(s, connection, session, link, event)
        .await
        .unwrap();
    let mut issued = Vec::new();
    while let Ok(command) = commands.try_recv() {
        issued.push(command);
    }
    issued
}

fn frame(value: &Value) -> Value {
    json!({ "type": "message", "text": value.to_string() })
}

fn connected_to(commands: &[socket::Command]) -> (String, Duration) {
    commands
        .iter()
        .find_map(|command| match command {
            socket::Command::Connect { url, after, .. } => Some((url.clone(), *after)),
            _ => None,
        })
        .expect("a connection")
}

fn sent(commands: &[socket::Command]) -> Vec<Value> {
    commands
        .iter()
        .filter_map(|command| match command {
            socket::Command::Send(text) => serde_json::from_str(text).ok(),
            _ => None,
        })
        .collect()
}

fn timer(commands: &[socket::Command], wanted: &str) -> Option<(Duration, Option<Duration>)> {
    commands.iter().find_map(|command| match command {
        socket::Command::Timer { name, at, every } if name == wanted => Some((*at, *every)),
        _ => None,
    })
}

fn closes(commands: &[socket::Command]) -> Vec<u16> {
    commands
        .iter()
        .filter_map(|command| match command {
            socket::Command::Close(code) => Some(*code),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn gateway_lifecycle_matches_discord_and_discord_js() {
    let (_root, s, agent) = service().await;
    let saved = save(&s, &agent, json!({ "guildId": "100", "channelId": "200" }))
        .await
        .unwrap();
    let connection: Connection = serde_json::from_value(saved).unwrap();
    let (origin, server, gateway) = rest_with_gateway().await;
    s.plugins.route_http_to(Some(origin));
    let package = registry::active(&s, "discord").await.unwrap();
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
    let (link, mut commands) = socket::Link::channel();
    let mut on = async |event: Value| {
        handle(&s, &connection, &mut session, &link, &mut commands, event).await
    };
    let hello = frame(&json!({ "op": 10, "d": { "heartbeat_interval": 41250 } }));

    // Start connects from a timer, to the URL GET /gateway/bot recommends.
    let issued = on(json!({ "type": "start", "settings": connection.settings })).await;
    assert_eq!(timer(&issued, "connect"), Some((Duration::ZERO, None)));
    let issued = on(json!({ "type": "tick", "name": "connect" })).await;
    let (url, after) = connected_to(&issued);
    assert_eq!(url, "wss://gateway-test.discord.gg/?v=10&encoding=json");
    assert_eq!(after, Duration::ZERO);
    assert_eq!(gateway.calls(), 1);

    // Hello within 60 s, then heartbeats with a random first beat and an Identify.
    let issued = on(json!({ "type": "open" })).await;
    assert_eq!(
        timer(&issued, "hello"),
        Some((Duration::from_secs(60), None))
    );
    let issued = on(hello.clone()).await;
    assert!(matches!(&issued[0], socket::Command::Cancel(name) if name == "hello"));
    let (first, every) = timer(&issued, "heartbeat").unwrap();
    assert!(first <= Duration::from_millis(41250));
    assert_eq!(every, Some(Duration::from_millis(41250)));
    let identify = &sent(&issued)[0];
    assert_eq!(identify["op"], 2);
    assert_eq!(identify["d"]["token"], "synthetic_discord_token");
    assert_eq!(
        timer(&issued, "ready"),
        Some((Duration::from_secs(15), None))
    );

    // No READY within 15 s: a new session, whose Identify waits five seconds after the
    // previous one for the same token.
    let issued = on(json!({ "type": "tick", "name": "ready" })).await;
    assert_eq!(closes(&issued), [4000]);
    connected_to(&issued);
    assert_eq!(gateway.calls(), 2);
    on(json!({ "type": "open" })).await;
    let started = std::time::Instant::now();
    let issued = on(hello.clone()).await;
    assert!(
        started.elapsed() >= Duration::from_millis(4500),
        "identify is paced"
    );
    assert_eq!(sent(&issued)[0]["op"], 2);
    let ready = frame(&json!({ "op": 0, "s": 1, "t": "READY", "d": {
        "session_id": "synthetic_session",
        "user": { "id": "400", "username": "Cairn", "bot": true },
        "resume_gateway_url": "wss://gateway-us-east1-b.discord.gg",
    } }));
    let issued = on(ready).await;
    assert!(matches!(&issued[0], socket::Command::Cancel(name) if name == "ready"));

    // Zombie: a second beat without acknowledgement reconnects and keeps the session.
    let issued = on(json!({ "type": "tick", "name": "heartbeat" })).await;
    assert_eq!(sent(&issued), [json!({ "op": 1, "d": 1 })]);
    let issued = on(json!({ "type": "tick", "name": "heartbeat" })).await;
    assert_eq!(closes(&issued), [4000]);
    let (url, _) = connected_to(&issued);
    assert_eq!(
        url,
        "wss://gateway-us-east1-b.discord.gg/?v=10&encoding=json"
    );

    // Invalid session that can be resumed: wait 1 to 5 s, then Resume.
    let issued = on(frame(&json!({ "op": 9, "d": true }))).await;
    let (url, after) = connected_to(&issued);
    assert!(url.starts_with("wss://gateway-us-east1-b"));
    assert!((Duration::from_secs(1)..=Duration::from_secs(5)).contains(&after));
    on(json!({ "type": "open" })).await;
    let issued = on(hello.clone()).await;
    assert_eq!(
        sent(&issued)[0],
        json!({ "op": 6, "d": { "token": "synthetic_discord_token", "session_id": "synthetic_session", "seq": 1 } })
    );
    assert_eq!(timer(&issued, "ready"), None);

    // Connected but no Hello: reconnect.
    on(json!({ "type": "open" })).await;
    let issued = on(json!({ "type": "tick", "name": "hello" })).await;
    assert_eq!(closes(&issued), [4000]);
    connected_to(&issued);

    // Close codes: 4000 resumes, 4009 starts a new session, 4014 stops the bot.
    let issued = on(json!({ "type": "close", "code": 4000, "reason": "" })).await;
    assert!(
        connected_to(&issued)
            .0
            .starts_with("wss://gateway-us-east1-b")
    );
    let issued = on(json!({ "type": "close", "code": 4009, "reason": "" })).await;
    assert!(connected_to(&issued).0.starts_with("wss://gateway-test"));

    // Invalid session that can't be resumed: a new session after 1 to 5 s.
    let issued = on(frame(&json!({ "op": 9, "d": false }))).await;
    let (url, after) = connected_to(&issued);
    assert!(url.starts_with("wss://gateway-test"));
    assert!(after >= Duration::from_secs(1));

    // Today's session starts used up: wait for Discord's reset rather than burn the token.
    gateway.remaining(0, 7_200_000);
    let issued = on(json!({ "type": "close", "code": 1000, "reason": "" })).await;
    assert_eq!(connected_to(&issued).1, Duration::from_secs(7200));
    let status = s
        .store
        .kv(&format!("plugin-status:{}", connection.id))
        .await
        .unwrap()
        .unwrap();
    assert!(text(&status, "message").starts_with("Discord’s daily session limit is reached"));

    let refused = socket::deliver(
        &s,
        &connection,
        &mut session,
        &link,
        json!({ "type": "close", "code": 4014, "reason": "" }),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.status, 403);
    session.stop().await;
    server.abort();
}

#[tokio::test]
async fn oversized_gateway_payloads_keep_the_session_and_fetch_threads() {
    let (_root, s, agent) = service().await;
    let saved = save(
        &s,
        &agent,
        json!({ "guildId": "100", "channelId": "200", "messageContent": true }),
    )
    .await
    .unwrap();
    let connection: Connection = serde_json::from_value(saved).unwrap();
    let (origin, server, _) = rest_with_gateway().await;
    s.plugins.route_http_to(Some(origin));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    s.plugins
        .route_sockets_to(Some(format!("ws://{}", listener.local_addr().unwrap())));
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        send(
            &mut socket,
            json!({ "op": 10, "d": { "heartbeat_interval": 30000 } }),
        )
        .await;
        assert_eq!(receive(&mut socket).await["op"], 2);
        send(
            &mut socket,
            json!({ "op": 0, "s": 1, "t": "READY", "d": {
                "session_id": "synthetic_session",
                "user": { "id": "400", "username": "Cairn", "bot": true },
                "resume_gateway_url": "wss://gateway-us-east1-b.discord.gg",
            } }),
        )
        .await;
        // A large server's GUILD_CREATE, well over what a script receives whole.
        let channels = (0..12_000)
            .map(|index| json!({ "id": format!("{}", 10_000 + index), "type": 0, "name": "x".repeat(80) }))
            .collect::<Vec<_>>();
        let large = json!({ "op": 0, "s": 2, "t": "GUILD_CREATE", "d": {
            "id": "100", "channels": channels, "threads": [],
        } });
        assert!(large.to_string().len() > 1_048_576);
        send(&mut socket, large).await;
        // A reply in a thread the script only knows from the REST fallback.
        send(
            &mut socket,
            json!({ "op": 0, "s": 3, "t": "MESSAGE_CREATE", "d": {
                "id": "501", "guild_id": "100", "channel_id": "700", "content": "and staging?",
                "author": { "id": "300", "username": "sarah" }, "mentions": [],
            } }),
        )
        .await;
        send(&mut socket, json!({ "op": 1 })).await;
        let heartbeat = receive(&mut socket).await;
        assert_eq!(
            heartbeat,
            json!({ "op": 1, "d": 3 }),
            "the session followed the large payload"
        );
        socket
            .close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: 4004.into(),
                reason: "".into(),
            }))
            .await
            .unwrap();
    });
    let package = registry::active(&s, "discord").await.unwrap();
    let stop = CancellationToken::new();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        socket::run(&s, &connection, &package, &stop),
    )
    .await
    .unwrap();
    upstream.await.unwrap();
    assert_eq!(result.unwrap_err().status, 403);
    let events = s.store.list(work::EVENTS).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"]["threadId"], "700");
    server.abort();
}
