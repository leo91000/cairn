mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    config::MAIN_AGENT_ID,
    plugins::{
        discord::{self, Connection},
        package,
    },
    service::Service,
    validation::text,
};
use cairn_plugin_host::Host;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;

async fn fixture() -> (TempDir, Arc<Service>, Connection) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".into();
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    let agent = s
        .agent(
            json!({
                "name": "Discord agent",
                "access": {
                    "projects": [],
                    "skills": [],
                    "mcps": [],
                    "github": false,
                    "installation": false
                },

            }),
            None,
        )
        .await
        .unwrap();
    let connection = discord::save(
        &s,
        &json!({
            "name": "Test Discord",
            "guildId": "100",
            "channelId": "200",
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

fn message(id: &str) -> Value {
    json!({
        "id": id,
        "guild_id": "100",
        "channel_id": "200",
        "content": "<@400> hello",
        "author": {
            "id": "300",
            "bot": false
        },
        "mentions": [{
            "id": "400"
        }],

    })
}

#[tokio::test]
async fn isolated_worker_enforces_api_capabilities_and_sanitizes_script_errors() {
    let host = Host::default();
    let executable = Path::new(env!("CARGO_BIN_EXE_cairn"));
    let (manifest, source) = package("onepassword").unwrap();
    let value = host
        .invoke(
            executable,
            &manifest,
            source,
            json!({
                "operation": "read"
            }),
            |method, input| async move {
                assert_eq!(method, "onepassword.read");
                assert_eq!(input["operation"], "read");
                Ok(json!({
                    "value": "synthetic_secret"
                }))
            },
        )
        .await
        .unwrap();
    assert_eq!(value["value"], "synthetic_secret");

    let calls = AtomicUsize::new(0);
    let result = host
        .invoke(
            executable,
            &manifest,
            "return function(input) return cairn.call('native.execute', input) end",
            Value::Null,
            |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok(Value::Null) }
            },
        )
        .await
        .unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(result.contains("capability"));
    let result = host
        .invoke(
            executable,
            &manifest,
            "return function(input) error(input.secret) end",
            json!({
                "secret": "synthetic_secret"
            }),
            |_, _| async { unreachable!() },
        )
        .await
        .unwrap_err();
    assert!(!result.contains("synthetic_secret"));
    let mut incompatible = manifest.clone();
    incompatible.api_version += 1;
    assert!(
        host.invoke(
            executable,
            &incompatible,
            source,
            Value::Null,
            |_, _| async { unreachable!() }
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn discord_ingress_requires_all_grants_and_deduplicates_across_restarts() {
    let (root, s, connection) = fixture().await;
    for mut denied in [
        message("501"),
        message("502"),
        message("503"),
        message("504"),
        message("505"),
    ] {
        match text(&denied, "id") {
            "501" => denied["guild_id"] = "999".into(),
            "502" => denied["channel_id"] = "999".into(),
            "503" => denied["author"]["id"] = "999".into(),
            "504" => denied["author"]["bot"] = true.into(),
            _ => denied["mentions"] = json!([]),
        }
        assert!(
            !discord::accept(&s, &connection, "400", &denied)
                .await
                .unwrap()
        );
    }
    let external = message("500");
    assert!(
        discord::accept(&s, &connection, "400", &external)
            .await
            .unwrap()
    );
    assert!(
        !discord::accept(&s, &connection, "400", &external)
            .await
            .unwrap()
    );
    // The manager can disappear between receiving the event and invoking the script.
    drop(s);
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    discord::process_pending(&s, &connection, Path::new(env!("CARGO_BIN_EXE_cairn")))
        .await
        .unwrap();
    assert!(s.store.list("plugin-inbox").await.unwrap().is_empty());
    let deliveries = s.store.list("plugin-deliveries").await.unwrap();
    assert_eq!(deliveries.len(), 1);
    let receipt = discord::submit(&s, &connection, &external, "hello")
        .await
        .unwrap();
    assert_eq!(receipt["messageId"], deliveries[0]["id"]);
    assert_eq!(s.store.list("chats").await.unwrap().len(), 1);
    assert!(
        !serde_json::to_string(&s.store.list(discord::KIND).await.unwrap())
            .unwrap()
            .contains("synthetic_discord_token")
    );
}

#[tokio::test]
async fn discord_access_requests_wait_for_owner_approval_without_storing_content() {
    let (_root, s, connection) = fixture().await;
    let saved = discord::save(
        &s,
        &json!({
            "name": "#support on Acme",
            "guildId": "100",
            "guildName": "Acme",
            "channelId": "200",
            "channelName": "support",
            "accessMode": "requests",
            "userIds": [],
            "bot": { "id": "400", "name": "Cairn Bot" },
            "agentId": connection.agent_id,
            "projectId": null,
            "maxMessagesPerHour": 20,
            "enabled": true,
        }),
        Some(&connection.id),
    )
    .await
    .unwrap();
    let connection: Connection = serde_json::from_value(saved).unwrap();
    let from = |id: &str, user: &str, name: &str| {
        let mut value = message(id);
        value["author"] = json!({ "id": user, "username": name.to_lowercase(), "global_name": name, "bot": false });
        value["content"] = json!("<@400> private question");
        value
    };
    for id in ["600", "601"] {
        assert!(
            !discord::accept(&s, &connection, "400", &from(id, "301", "Sarah"))
                .await
                .unwrap()
        );
    }
    let requests = s.store.list("plugin-requests").await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["name"], "Sarah");
    assert_eq!(requests[0]["mentions"], 2);
    // Only the first mention is answered, and nothing they wrote is kept.
    let deliveries = s.store.list("plugin-deliveries").await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert!(s.store.list("plugin-inbox").await.unwrap().is_empty());
    for kind in ["plugin-requests", "plugin-deliveries"] {
        let stored = serde_json::to_string(&s.store.list(kind).await.unwrap()).unwrap();
        assert!(!stored.contains("private question"));
    }
    let notice = discord::reply(&s, &connection, &deliveries[0])
        .await
        .unwrap()
        .unwrap();
    assert!(notice.contains("approve"));

    // Approval applies to the running Gateway session without a new revision.
    discord::people(&s, &connection.id, "301", "approve")
        .await
        .unwrap();
    assert!(
        discord::accept(&s, &connection, "400", &from("602", "301", "Sarah"))
            .await
            .unwrap()
    );
    let mut stored = from("603", "301", "Sarah");
    stored["author"]["name"] = json!("Sarah");
    discord::submit(&s, &connection, &stored, "private question")
        .await
        .unwrap();

    assert!(
        !discord::accept(&s, &connection, "400", &from("610", "302", "Max"))
            .await
            .unwrap()
    );
    discord::people(&s, &connection.id, "302", "deny")
        .await
        .unwrap();
    assert!(
        !discord::accept(&s, &connection, "400", &from("611", "302", "Max"))
            .await
            .unwrap()
    );
    assert_eq!(s.store.list("plugin-deliveries").await.unwrap().len(), 3);

    let overview = discord::overview(&s).await.unwrap();
    let view = &overview[0];
    assert_eq!(view["userIds"], json!(["301"]));
    assert_eq!(view["people"], json!([{ "id": "301", "name": "Sarah" }]));
    assert_eq!(view["requests"], json!([]));
    assert_eq!(view["usage"]["hour"], 1);
    assert_eq!(view["activity"][0]["name"], "Sarah");
    assert_eq!(view["activity"][0]["status"], "working");

    discord::people(&s, &connection.id, "301", "remove")
        .await
        .unwrap();
    assert!(
        !discord::accept(&s, &connection, "400", &from("604", "301", "Sarah"))
            .await
            .unwrap()
    );
    discord::set_enabled(&s, &connection.id, &json!({ "enabled": false }))
        .await
        .unwrap();
    assert_eq!(
        discord::accept(&s, &connection, "400", &from("605", "301", "Sarah"))
            .await
            .unwrap_err()
            .status,
        403
    );
    discord::remove(&s, &connection.id).await.unwrap();
    for kind in ["plugin-requests", "plugin-activity", "plugin-deliveries"] {
        assert!(s.store.list(kind).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn discord_rate_limits_and_changed_agent_access_stop_future_admission() {
    let (_root, s, connection) = fixture().await;
    discord::submit(&s, &connection, &message("500"), "hello")
        .await
        .unwrap();
    discord::submit(&s, &connection, &message("501"), "next")
        .await
        .unwrap();
    assert_eq!(
        discord::submit(&s, &connection, &message("502"), "too many")
            .await
            .unwrap_err()
            .status,
        429
    );
    let mut agent = s.get("agents", &connection.agent_id).await.unwrap();
    agent["access"]["projects"] = Value::Null;
    s.agent(agent, Some(&connection.agent_id)).await.unwrap();
    assert_eq!(
        discord::accept(&s, &connection, "400", &message("503"))
            .await
            .unwrap_err()
            .status,
        403
    );
    assert!(
        discord::save(
            &s,
            &json!({
                "name": "Broad",
                "guildId": "100",
                "channelId": "200",
                "userIds": ["300"],
                "agentId": MAIN_AGENT_ID,
                "projectId": null,
                "maxMessagesPerHour": 20,
                "enabled": true,
                "token": "synthetic_discord_token",

            }),
            None
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn discord_replies_export_only_the_correlated_completed_turn_and_revoke_immediately() {
    let (_root, s, connection) = fixture().await;
    let receipt = discord::submit(&s, &connection, &message("500"), "hello")
        .await
        .unwrap();
    // Prepare the conversation's run as normal admission does, without starting a real agent.
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.get("chats", text(&receipt, "chatId")).await.unwrap();
    let run_id = text(&chat, "runId");
    s.store
        .event(
            run_id,
            "item.completed",
            "unrelated answer",
            Some(json!({
                "item": {
                    "type": "agent_message",
                    "text": "unrelated answer"
                }
            })),
        )
        .await
        .unwrap();
    s.store
        .event(run_id, "turn.completed", "", None)
        .await
        .unwrap();
    let delivery = s.store.list("plugin-deliveries").await.unwrap().remove(0);
    assert!(
        discord::reply(&s, &connection, &delivery)
            .await
            .unwrap()
            .is_none()
    );
    s.store
        .event(
            run_id,
            "chat.user",
            "hello",
            Some(json!({
                "messageId": receipt["messageId"]
            })),
        )
        .await
        .unwrap();
    s.store
        .event(
            run_id,
            "item.completed",
            "synthetic_secret",
            Some(json!({
                "item": {
                    "type": "command_execution",
                    "aggregated_output": "synthetic_secret"
                }
            })),
        )
        .await
        .unwrap();
    s.store
        .event(
            run_id,
            "item.completed",
            "hello back",
            Some(json!({
                "item": {
                    "type": "agent_message",
                    "text": "hello back"
                }
            })),
        )
        .await
        .unwrap();
    assert!(
        discord::reply(&s, &connection, &delivery)
            .await
            .unwrap()
            .is_none()
    );
    s.store
        .event(run_id, "turn.completed", "", None)
        .await
        .unwrap();
    assert_eq!(
        discord::reply(&s, &connection, &delivery)
            .await
            .unwrap()
            .unwrap(),
        "hello back"
    );
    discord::remove(&s, &connection.id).await.unwrap();
    assert_eq!(
        discord::reply(&s, &connection, &delivery)
            .await
            .unwrap_err()
            .status,
        403
    );
    assert!(s.store.list("plugin-deliveries").await.unwrap().is_empty());
    assert!(s.get("chats", text(&receipt, "chatId")).await.is_ok());
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
            json!({
                "name": "Inspect",
                "agentId": MAIN_AGENT_ID,
                "prompt": "Inspect"
            }),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&run, "id"),
            json!({
                "status": "running",
                "isolated": true
            }),
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
                    "params": {
                        "name": "onepassword",
                        "arguments": {
                            "operation": "read"
                        }
                    }
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
                json!({
                    "plugin": "onepassword",
                    "arguments": {
                        "operation": "read"
                    }
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn worker_deadline_terminates_scripts_even_when_they_catch_interrupt_errors() {
    let host = Host::with_limits(1, std::time::Duration::from_millis(200));
    let (manifest, _) = package("onepassword").unwrap();
    let started = std::time::Instant::now();
    let error = host
        .invoke(
            Path::new(env!("CARGO_BIN_EXE_cairn")),
            &manifest,
            "return function(input) while true do pcall(function() while true do end end) end end",
            Value::Null,
            |_, _| async { unreachable!() },
        )
        .await
        .unwrap_err();
    assert!(error.contains("deadline"));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    // A timed-out worker releases its slot and its private workspace.
    let result = host
        .invoke(
            Path::new(env!("CARGO_BIN_EXE_cairn")),
            &manifest,
            "return function(input) return true end",
            Value::Null,
            |_, _| async { unreachable!() },
        )
        .await
        .unwrap();
    assert_eq!(result, true);
}

#[tokio::test]
async fn private_plugin_channel_rejects_other_runs_and_cancelled_grants() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn grant(s: &Service, agent: &str) -> (Value, String) {
        let task = s
            .task(
                json!({"name": "Inspect", "agentId": agent, "prompt": "Inspect"}),
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
        let configuration = s.mcps.run_configuration(s, &run).await.unwrap();
        (
            run,
            text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN").to_owned(),
        )
    }
    async fn request(socket: &Path, token: &str) -> String {
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        let body = json!({"plugin": "missing", "arguments": {}}).to_string();
        let request = format!(
            "POST /plugin-command HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    }
    let (root, s, connection) = fixture().await;
    let (run, token) = grant(&s, &connection.agent_id).await;
    let (_, other) = grant(&s, &connection.agent_id).await;
    let _channel =
        cairn_installation::mcps::channel::serve(&s, &root.path().join("home"), text(&run, "id"))
            .await
            .unwrap();
    let socket = root.path().join("home/cairn-mcp.sock");
    assert!(request(&socket, &other).await.starts_with("HTTP/1.1 401"));
    assert!(request(&socket, &token).await.starts_with("HTTP/1.1 404"));
    s.store
        .patch_run(
            text(&run, "id"),
            json!({"cancelRequestedAt": cairn_installation::config::now()}),
        )
        .await
        .unwrap();
    assert!(request(&socket, &token).await.starts_with("HTTP/1.1 401"));
}
