mod common;

use common::eventually;
use leo_agent_manager::{
    accounts::{
        KIND, Lease, broker,
        codex::{Client, normalize},
        usage::{blocked, recovered, remaining},
    },
    chat_process,
    config::{Config, id, now},
    models::cached_defaults,
    provider::Provider,
    rpc::Session,
    service::Service,
};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const FIRST_RUN: &str = "11111111-1111-4111-8111-111111111111";
const SECOND_RUN: &str = "22222222-2222-4222-8222-222222222222";

#[tokio::test]
async fn chat_binary_correlates_native_rpc_and_activity_without_changing_stdout_or_logging_prompts()
{
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let root = TempDir::new().unwrap();
    let config = config(&root);
    let config_file = root.path().join("config.json");
    std::fs::write(&config_file, serde_json::to_vec(&config).unwrap()).unwrap();
    let home = codex_home(&config);
    let claude_home = root.path().join("claude");
    std::fs::create_dir_all(&claude_home).unwrap();
    std::fs::write(claude_home.join(".credentials.json"), "fixture").unwrap();
    for provider in ["codex", "claude"] {
        let mut output_types = Vec::new();
        for correlated in [false, true] {
            let mut plan = json!({
                "provider": provider,
                "cwd": root.path(),
                "model": "fixture",
                "reasoning": "medium",
                "sandbox": "yolo",
                "output": root.path().join(format!("reply-{provider}-{correlated}.md")),
                "inputDirectory": root.path(),
                "writableRoots": [],
                "execution": {
                    "messageId": "fixture-message",
                    "text": "PRIVATE_PROMPT_MARKER",
                    "attachments": [],
                },
            });
            if correlated {
                plan["runId"] = FIRST_RUN.into();
                plan["attemptId"] = SECOND_RUN.into();
            }
            let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_leo"))
                .args(["chat", &common::fixture(&format!("{provider}.mjs"))])
                .env("LEO_CONFIG", &config_file)
                .env("CODEX_HOME", &home)
                .env("CLAUDE_CONFIG_DIR", root.path().join("claude"))
                .env(
                    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
                    root.path().join("claude"),
                )
                .env("RUST_LOG", "warn,leo_performance=info")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(plan.to_string().as_bytes()).await.unwrap();
            drop(stdin);
            let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{stderr}");
            if provider == "codex" {
                assert!(stderr.contains("codex_rpc"));
                assert!(stderr.contains("initialize"));
            } else {
                assert!(stderr.contains("thread_initialized"));
            }
            assert!(stderr.contains("agent_activity"));
            assert!(stderr.contains("first_message"));
            assert!(!stderr.contains("PRIVATE_PROMPT_MARKER"));
            assert!(!stderr.contains("rg --files src/components"));
            assert!(!stderr.contains(&root.path().to_string_lossy().into_owned()));
            if correlated {
                assert!(stderr.contains(FIRST_RUN));
                assert!(stderr.contains(SECOND_RUN));
            } else {
                assert!(stderr.contains("unknown"));
            }
            let events = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(events.last().unwrap()["type"], "turn.completed");
            assert!(!events.iter().any(|event| event["type"] == "diagnostic"));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event["type"] == "chat.delivered")
                    .count(),
                1
            );
            output_types.push(
                events
                    .iter()
                    // Delivery acknowledgement races the turn notification in
                    // the native protocol; compare the ordered output stream.
                    .filter(|event| event["type"] != "chat.delivered")
                    .map(|event| event["type"].clone())
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(
            output_types[0], output_types[1],
            "old plans and correlated plans keep the same protocol"
        );
    }
}

/// A service whose Codex binary is the repository fixture.
fn config(root: &TempDir) -> Config {
    Config {
        codex_bin: common::fixture("codex.mjs"),
        ..common::config(root.path())
    }
}

/// Leases a Codex account for `run`; the pool must have one available.
async fn acquire(service: &Service, run: &str, model: &str) -> Lease {
    service
        .accounts
        .acquire(service, run, Provider::Codex, model)
        .await
        .unwrap()
        .unwrap()
}

/// Creates a Codex account holding synthetic credentials and refreshes its usage.
async fn refreshed_account(service: &Service, name: &str, tokens: Value) -> String {
    let account = service
        .accounts
        .create(service, Provider::Codex, name)
        .await
        .unwrap();
    let id = account["id"].as_str().unwrap().to_owned();
    service
        .vault
        .set(&format!("codex-account:{id}"), &json!({ "tokens": tokens }))
        .await
        .unwrap();
    service.accounts.refresh(service, &id).await.unwrap();
    id
}

fn codex_home(config: &Config) -> std::path::PathBuf {
    let home = config.home.join(".codex");
    std::fs::create_dir_all(&home).unwrap();
    home
}

#[tokio::test]
async fn codex_sessions_keep_managed_mcp_configuration_without_cloning_native_plugins() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let home = codex_home(&config);
    let launch = home.join("fixture-launch-args.json");
    std::fs::write(&launch, "[]").unwrap();
    let managed_mcp = "mcp_servers.fixture.url=\"https://example.invalid/mcp\"";
    let args = ["-c".into(), managed_mcp.into()];
    let session = Session::codex(&config, &home, &args, Some(root.path()))
        .await
        .unwrap();
    session.close().await;

    let launched: Vec<String> = serde_json::from_slice(&std::fs::read(launch).unwrap()).unwrap();
    assert!(launched.windows(2).any(|pair| pair == ["-c", managed_mcp]));
    assert!(
        launched
            .windows(2)
            .any(|pair| pair == ["-c", "features.plugins=false"])
    );
}

#[tokio::test]
async fn failed_initialization_waits_for_the_codex_process_to_exit() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let home = codex_home(&config);
    let marker = home.join("fixture-initialize-error");
    std::fs::write(&marker, "").unwrap();
    let result = Session::codex(&config, &home, &[], Some(root.path())).await;
    assert!(result.is_err());
    let pid: i32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    if alive {
        // Leave no fixture process behind when checking the pre-fix failure.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    assert!(!alive, "failed initialization returned before Codex exited");
}

#[tokio::test]
async fn long_chat_history_preserves_steered_messages_without_repeating_completed_work() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let home = codex_home(&config);
    let file = home.join("fixture-conversation.json");
    let command = |id: String| json!({ "id": id, "type": "commandExecution", "aggregatedOutput": "x".repeat(2_100_000) });
    let user = |id: &str, client: &str, text: &str| {
        json!({
            "id": id,
            "type": "userMessage",
            "clientId": client,
            "content": [{ "type": "text", "text": text }],
        })
    };
    let mut items = vec![
        user("u", "original", "Old request"),
        command("c".into()),
        user("s", "steered", "Steer request"),
        json!({ "id": "a", "type": "agentMessage", "text": "Completed once" }),
    ];
    // Item pagination recovers a large turn without loading all tool output at once.
    for index in 0..16 {
        items.insert(1, command(format!("command-{index}")));
    }
    let mut turns = (0..101)
        .rev()
        .map(|index| json!({ "id": format!("older-{index}"), "status": "completed", "items": [] }))
        .collect::<Vec<_>>();
    turns.push(json!({ "id": "old", "status": "completed", "items": items }));
    let history = json!({ "id": "fixture-chat", "historyMode": "paginated", "turns": turns });
    std::fs::write(&file, history.to_string()).unwrap();
    for (message_id, expected_turns) in [("steered", 102), ("new-message", 103)] {
        let (events, mut receiver) = tokio::sync::mpsc::channel(64);
        let plan = json!({
            "args": [],
            "cwd": root.path(),
            "model": "fixture",
            "reasoning": "medium",
            "sandbox": "yolo",
            "sessionId": "fixture-chat",
            "output": root.path().join("reply.md"),
            "inputDirectory": root.path(),
            "writableRoots": [],
            "execution": {
                "messageId": message_id,
                "text": "New request",
                "attachments": [],
                "recovery": true,
            },
        });
        chat_process::run(&config, &home, plan, events, CancellationToken::new())
            .await
            .unwrap();
        let mut delivered = false;
        while let Some(event) = receiver.recv().await {
            delivered |= event["type"] == "chat.delivered" && event["messageId"] == message_id;
        }
        assert!(delivered);
        let history: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(history["turns"].as_array().unwrap().len(), expected_turns);
        if message_id == "steered" {
            assert_eq!(
                std::fs::read_to_string(root.path().join("reply.md")).unwrap(),
                "Completed once"
            );
        }
    }
}

#[tokio::test]
async fn resume_recovers_items_whose_turn_metadata_is_missing() {
    // Production had one listed in-progress turn and 53 items belonging to an
    // unlisted continuation. Keep the same mismatch without private history.
    for (unlisted_first, listed_status, message_id, recovery, expected_new_turn) in [
        (false, "inProgress", "original", true, true),
        (false, "completed", "original", true, true),
        (false, "completed", "steered", true, true),
        (false, "completed", "steered", false, true),
        (true, "completed", "original", true, false),
        (false, "completed", "new-message", false, true),
    ] {
        let root = TempDir::new().unwrap();
        let config = config(&root);
        let home = codex_home(&config);
        let file = home.join("fixture-conversation.json");

        let listed = json!({
            "id": "listed",
            "status": listed_status,
            "items": [
                {
                    "id": "u",
                    "type": "userMessage",
                    "clientId": "original",
                    "content": [{ "type": "text", "text": "Original request" }],
                },
                { "id": "a", "type": "agentMessage", "text": "Earlier reply" },
            ],
        });
        let unlisted = json!({
            "id": "unlisted",
            "status": "inProgress",
            "items": [
                { "id": "tool", "type": "commandExecution", "aggregatedOutput": "Work already done" },
                {
                    "id": "steer",
                    "type": "userMessage",
                    "clientId": "steered",
                    "content": [{ "type": "text", "text": "Steered request" }],
                },
                { "id": "commentary", "type": "agentMessage", "text": "Still working" },
            ],
        });
        let turns = if unlisted_first {
            vec![unlisted, listed]
        } else {
            vec![listed, unlisted]
        };
        let history = json!({
            "id": "fixture-chat",
            "historyMode": "paginated",
            "fixtureUnlistedTurns": ["unlisted"],
            "turns": turns,
        });
        std::fs::write(&file, history.to_string()).unwrap();

        let (events, mut receiver) = tokio::sync::mpsc::channel(64);
        let plan = json!({
            "args": [],
            "cwd": root.path(),
            "model": "fixture",
            "reasoning": "medium",
            "sandbox": "yolo",
            "sessionId": "fixture-chat",
            "output": root.path().join("reply.md"),
            "inputDirectory": root.path(),
            "writableRoots": [],
            "execution": {
                "messageId": message_id,
                "text": "Pending request",
                "attachments": [],
                "recovery": recovery,
            },
        });
        chat_process::run(&config, &home, plan, events, CancellationToken::new())
            .await
            .unwrap();

        let mut receipts = Vec::new();
        while let Some(event) = receiver.recv().await {
            if event["type"] == "chat.delivered" {
                receipts.push(event["messageId"].as_str().unwrap().to_owned());
            }
        }
        for id in ["original", "steered", message_id] {
            assert_eq!(receipts.iter().filter(|receipt| *receipt == id).count(), 1);
        }

        let history: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let turns = history["turns"].as_array().unwrap();
        assert_eq!(turns.len(), if expected_new_turn { 3 } else { 2 });
        if !expected_new_turn {
            assert_eq!(
                std::fs::read_to_string(root.path().join("reply.md")).unwrap(),
                "Earlier reply"
            );
            continue;
        }

        let input = &turns.last().unwrap()["items"][0];
        if message_id == "new-message" {
            assert_eq!(input["clientId"], message_id);
            assert_eq!(input["content"][0]["text"], "Pending request");
            continue;
        }

        assert!(
            input["clientId"].is_null(),
            "do not deliver an accepted user message twice"
        );
        assert!(
            input["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("Continue the interrupted conversation")
        );
    }
}

/// Reconnects a Codex account whose sign-in follows the fixture's `mode`.
async fn sign_in(mode: &str) -> (TempDir, Arc<Service>, String) {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    service.accounts.initialize(&service).await.unwrap();
    let account = service
        .accounts
        .create(&service, Provider::Codex, "Personal")
        .await
        .unwrap();
    let id = account["id"].as_str().unwrap().to_owned();
    let home = service
        .config
        .data_dir
        .join("account-login")
        .join(&id)
        .join(".codex");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("fixture-login.json"),
        json!({ "mode": mode }).to_string(),
    )
    .unwrap();
    let view = service.accounts.reconnect(&service, &id).await.unwrap();
    assert_eq!(view["provider"], "codex");
    (root, service, id)
}

/// Waits until the sign-in view satisfies `condition`.
async fn sign_in_until(service: &Service, condition: impl Fn(&Value) -> bool) -> Value {
    eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || {
            let view = service.accounts.sign_in().await;
            condition(&view).then_some(view)
        },
    )
    .await
}

fn is_settled(view: &Value) -> bool {
    view["state"] != "pending"
}

#[tokio::test]
async fn oversized_mcp_frames_report_the_transport_limit_not_an_auth_failure() {
    let limit = 2_000_000;
    let mut command = tokio::process::Command::new("node");
    command.args(["-e", &format!("process.stdin.once('data', () => process.stdout.write(JSON.stringify({{id:1,result:'x'.repeat({limit})}})+'\\n')); setInterval(()=>{{}},1000)")]);
    let session = Session::spawn_with_protocol(command, true).await.unwrap();
    let error = session.rpc.request("large", json!({})).await.unwrap_err();
    assert!(
        error.message.contains(&format!("{limit}-byte limit")),
        "{error:?}"
    );
    session.close().await;
}

#[tokio::test]
async fn codex_accepts_large_responses_and_notifications_and_continues_reading() {
    let mut command = tokio::process::Command::new("node");
    command.args([
        "-e",
        r#"
        require('node:readline').createInterface({input:process.stdin}).on('line', line => {
            const {id, method} = JSON.parse(line);
            const text = method === 'large' ? 'x'.repeat(33_000_000) : 'still connected';
            process.stdout.write(JSON.stringify({id, result:text})+'\n');
            if (method === 'large') {
                process.stdout.write(JSON.stringify({method:'item/completed', params:{text}})+'\n');
            }
        });
    "#,
    ]);
    let mut session = Session::spawn(command).await.unwrap();
    let response = session.rpc.request("large", json!({})).await.unwrap();
    assert_eq!(response.as_str().unwrap(), "x".repeat(33_000_000));
    let notification = tokio::time::timeout(Duration::from_secs(10), session.incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notification.method, "item/completed");
    assert_eq!(notification.params["text"], response);
    assert_eq!(
        session.rpc.request("small", json!({})).await.unwrap(),
        "still connected"
    );
    session.close().await;
}

#[tokio::test]
async fn structured_sign_in_displays_the_code_and_cancelling_removes_the_unfinished_account() {
    let (_root, service, id) = sign_in("hold").await;
    let view = sign_in_until(&service, |v| v["phase"] == "authorizing").await;
    assert_eq!(view["code"], "ABCD-12345");
    assert_eq!(view["url"], "https://auth.openai.com/codex/device");
    assert_eq!(view["state"], "pending");
    assert_eq!(view["accountId"], id.as_str());
    assert!(view["expiresAt"].as_i64().unwrap() > now());
    service.accounts.cancel(&service).await.unwrap();
    assert!(service.accounts.sign_in().await.is_null());
    assert!(service.store.get(KIND, &id).await.unwrap().is_none());
    assert!(
        service
            .vault
            .get(&format!("codex-account:{id}"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !service
            .config
            .data_dir
            .join("account-login")
            .join(&id)
            .exists()
    );
}

#[tokio::test]
async fn structured_sign_in_handles_completion_before_the_start_reply() {
    let (_root, service, id) = sign_in("immediate").await;
    let view = sign_in_until(&service, is_settled).await;
    assert_eq!(view["state"], "complete", "{view}");
    assert!(view["code"].is_null());
    assert_eq!(
        service.accounts.get(&service, &id).await.unwrap()["state"],
        "ready"
    );
}

#[tokio::test]
async fn structured_sign_in_reports_errors_without_exposing_provider_details() {
    for mode in ["failure", "unsupported", "disconnect"] {
        let (_root, service, id) = sign_in(mode).await;
        let view = sign_in_until(&service, is_settled).await;
        assert_eq!(view["state"], "failed", "{mode}");
        assert!(view["code"].is_null());
        assert!(!view["error"].as_str().unwrap().is_empty());
        assert!(!view.to_string().contains("synthetic secret"));
        if mode == "unsupported" {
            assert!(view["error"].as_str().unwrap().contains("Update Codex"));
        }
        // A failed sign-in keeps the account so it can be retried.
        assert_eq!(
            service.accounts.get(&service, &id).await.unwrap()["state"],
            "pending"
        );
    }
}

#[tokio::test]
async fn completed_sign_in_is_already_cleaned_up_and_available() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    let flow = service
        .accounts
        .add(&service, Provider::Codex, "Personal")
        .await
        .unwrap();
    let id = flow["accountId"].as_str().unwrap();

    // Observe the publication boundary, rather than allowing a polling delay to
    // hide work that still happens after the UI is told sign-in is complete.
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let view = service.accounts.sign_in().await;
            if is_settled(&view) {
                assert_eq!(view["state"], "complete", "{view}");
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert!(
        !service
            .config
            .data_dir
            .join("account-login")
            .join(id)
            .exists()
    );
    assert!(!service.accounts.busy(&service, id).await.unwrap());
    assert_eq!(
        service.accounts.list(&service).await.unwrap()[0]["status"],
        "next"
    );

    service.accounts.reconnect(&service, id).await.unwrap();
    service.accounts.cancel(&service).await.unwrap();
}

#[tokio::test]
async fn account_sign_in_verifies_identity_captures_credentials_and_cleans_up() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    let view = service
        .accounts
        .add(&service, Provider::Codex, "Personal")
        .await
        .unwrap();
    let id = view["accountId"].as_str().unwrap().to_owned();
    assert_eq!(
        sign_in_until(&service, is_settled).await["state"],
        "complete"
    );
    assert_eq!(
        service.accounts.get(&service, &id).await.unwrap()["state"],
        "ready"
    );
    assert!(
        service
            .vault
            .get(&format!("codex-account:{id}"))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        !service
            .config
            .data_dir
            .join("account-login")
            .join(&id)
            .exists()
    );
    assert!(!service.accounts.busy(&service, &id).await.unwrap());
}

#[tokio::test]
async fn models_are_paginated_cached_and_keep_last_known_options_when_codex_is_down() {
    let root = TempDir::new().unwrap();
    let mut configuration = config(&root);
    let executable = root.path().join("codex");
    std::os::unix::fs::symlink(&configuration.codex_bin, &executable).unwrap();
    configuration.codex_bin = executable.to_string_lossy().into_owned();
    let service = Service::new(configuration).await.unwrap();
    let catalog = service.models.list(&service).await.unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 3);
    assert_eq!(catalog["stale"], false);
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["hidden"] == true)
    );
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["supportedReasoningEfforts"][1]["reasoningEffort"] == "ultra")
    );
    std::fs::remove_file(executable).unwrap();
    assert_eq!(service.models.list(&service).await.unwrap(), catalog);
    let mut cache = service.store.kv("codex-models:").await.unwrap().unwrap();
    cache["checkedAt"] = 1.into();
    cache["attemptedAt"] = 1.into();
    service
        .store
        .set("codex-models:", cache, None)
        .await
        .unwrap();
    let stale = service.models.list(&service).await.unwrap();
    assert_eq!(stale["models"], catalog["models"]);
    assert_eq!(stale["stale"], true);
    assert!(!stale["error"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn model_catalog_respects_enabled_accounts_and_routes_to_an_account_with_the_model() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    service.accounts.initialize(&service).await.unwrap();
    let mut ids = Vec::new();
    for name in ["fast-only", "all-models"] {
        let tokens = json!({ "access_token": "synthetic", "account_id": name });
        ids.push(refreshed_account(&service, name, tokens).await);
    }
    let catalog = service.models.list(&service).await.unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 3);
    assert_eq!(
        cached_defaults(&service, &ids[0], "fixture-deep")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        cached_defaults(&service, &ids[1], "fixture-deep")
            .await
            .unwrap(),
        Some(("fixture-deep".into(), "medium".into()))
    );
    assert_eq!(
        cached_defaults(&service, &ids[0], "").await.unwrap(),
        Some(("fixture-fast".into(), "low".into()))
    );
    for id in &ids {
        assert!(
            !service
                .config
                .data_dir
                .join("codex-model-discovery")
                .join(id)
                .exists()
        );
    }
    let lease = acquire(&service, FIRST_RUN, "fixture-deep").await;
    assert_eq!(lease.account_id, ids[1]);
    service.accounts.release(&lease).await.unwrap();
    service
        .accounts
        .update(&service, &ids[1], &json!({ "enabled": false }))
        .await
        .unwrap();
    let catalog = service.models.list(&service).await.unwrap();
    assert!(
        !catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["model"] == "fixture-deep")
    );
    service.accounts.remove(&service, &ids[0]).await.unwrap();
    assert!(
        service.models.list(&service).await.unwrap()["models"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn native_rpc_runs_a_turn_and_resumes_its_persisted_thread() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let home = root.path().join("codex");
    std::fs::create_dir_all(&home).unwrap();
    let mut session = Session::codex(&config, &home, &[], Some(root.path()))
        .await
        .unwrap();
    let started = session
        .request("thread/start", json!({ "cwd": root.path() }))
        .await
        .unwrap();
    assert_eq!(started["thread"]["id"], "fixture-chat");
    let turn = json!({
        "threadId": "fixture-chat",
        "input": [{ "type": "text", "text": "Check the workspace" }],
        "clientUserMessageId": "message-1",
    });
    session.request("turn/start", turn).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let incoming = session.incoming.recv().await.unwrap();
            if incoming.method == "turn/completed" {
                break;
            }
        }
    })
    .await
    .unwrap();
    session.close().await;
    let mut resumed = Session::codex(&config, &home, &[], Some(root.path()))
        .await
        .unwrap();
    resumed
        .request("thread/resume", json!({ "threadId": "fixture-chat" }))
        .await
        .unwrap();
    let turns = resumed
        .request(
            "thread/turns/list",
            json!({ "threadId": "fixture-chat", "limit": 100 }),
        )
        .await
        .unwrap();
    assert_eq!(turns["data"][0]["status"], "completed");
    assert_eq!(turns["data"][0]["items"][0]["clientId"], "message-1");
    resumed.close().await;
}

#[tokio::test]
async fn queued_reasoning_is_idempotent_editable_and_part_of_the_run_snapshot() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    let chat = service.chat_create(json!({})).await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let message_id = id();
    let mut message = json!({
        "id": message_id,
        "text": "Review this",
        "model": "fixture-deep",
        "reasoning": "ultra",
    });
    let first = service.chat_send(chat_id, message.clone()).await.unwrap();
    assert_eq!(
        service.chat_send(chat_id, message.clone()).await.unwrap(),
        first
    );
    message["reasoning"] = "medium".into();
    assert_eq!(
        service
            .chat_send(chat_id, message.clone())
            .await
            .unwrap_err()
            .status,
        409
    );
    service
        .chat_edit(chat_id, &message_id, Some(message))
        .await
        .unwrap();
    service.chat_tick(&HashSet::new()).await.unwrap();
    let detail = service.chat_detail(chat_id).await.unwrap();
    assert!(
        detail["run"].is_object(),
        "{:?}",
        service
            .store
            .kv(&format!("chat-error:{chat_id}"))
            .await
            .unwrap()
    );
    assert_eq!(detail["run"]["snapshot"]["agent"]["reasoning"], "medium");
    assert_eq!(detail["run"]["snapshot"]["agent"]["model"], "fixture-deep");
    let steer = json!({ "id": id(), "text": "More detail", "mode": "steer", "reasoning": "ultra" });
    assert_eq!(
        service.chat_send(chat_id, steer).await.unwrap_err().status,
        409
    );
}

#[tokio::test]
async fn device_login_verifies_identity_then_leases_private_credentials() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    let flow = service
        .accounts
        .add(&service, Provider::Codex, "Test account")
        .await
        .unwrap();
    let id = flow["accountId"].as_str().unwrap();
    let view = sign_in_until(&service, is_settled).await;
    assert_eq!(view["state"], "complete", "{view}");
    let accounts = service.accounts.list(&service).await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["remainingPercent"], 60.0);
    assert_eq!(accounts[0]["provider"], "codex");
    assert_eq!(accounts[0]["status"], "next");
    assert!(accounts[0].get("identity").is_none());
    let lease = acquire(&service, FIRST_RUN, "").await;
    assert_eq!(lease.account_id, id);
    assert!(!lease.home.join("auth.json").exists());
    assert!(lease.home.join("leo-managed-auth").exists());
    assert!(service.accounts.remove(&service, id).await.is_err());
    let second = acquire(&service, SECOND_RUN, "").await;
    assert_eq!(second.account_id, lease.account_id);
    service.accounts.release(&lease).await.unwrap();
    assert_eq!(service.accounts.active(id).await.len(), 1);
    assert!(service.accounts.remove(&service, id).await.is_err());
    service.accounts.release(&second).await.unwrap();
    assert!(!lease.home.join("auth.json").exists());
    service.accounts.remove(&service, id).await.unwrap();
    assert!(service.accounts.list(&service).await.unwrap().is_empty());
}

#[test]
fn codex_usage_requires_observed_capacity_and_respects_model_limits() {
    let before = json!({
        "ordinaryUsageAllowed": true,
        "rateLimits": {
            "primary": { "usedPercent": 100, "resetsAt": 1 },
            "secondary": { "usedPercent": 40 },
        },
        "rateLimitsByLimitId": {
            "model": {
                "limitId": "special",
                "normalModelSlug": "model-x",
                "primary": { "usedPercent": 95 },
            },
        },
        "rateLimitResetCredits": { "availableCount": 2 },
    });
    assert_eq!(remaining(&normalize(&before), ""), Some(0.));
    assert_eq!(normalize(&before)["resets"]["available"], 2);
    let mut after = before.clone();
    after["rateLimits"]["primary"]["resetsAt"] = 9_999_999_999_i64.into();
    assert!(!recovered(&normalize(&before), &normalize(&after), ""));
    after["rateLimits"]["primary"]["usedPercent"] = 0.into();
    assert!(recovered(&normalize(&before), &normalize(&after), ""));
    assert_eq!(remaining(&normalize(&after), ""), Some(60.));
    assert_eq!(remaining(&normalize(&after), "model-x"), Some(5.));
    assert_eq!(remaining(&normalize(&after), "special"), Some(5.));
    after["rateLimitsByLimitId"]["model"]["spendControlReached"] = true.into();
    assert!(blocked(&normalize(&after), "model-x"));
    assert!(!blocked(&normalize(&after), ""));
    after["ordinaryUsageAllowed"] = false.into();
    assert!(blocked(&normalize(&after), ""));
}

/// Two runs share one account: refreshes are serialized and neither run can
/// forge the other's lease.
async fn assert_refreshes_are_shared(
    service: &Service,
    account: &str,
    first: &Lease,
    second: &Lease,
) {
    let mut a = Client::new(&first.home).unwrap();
    let mut b = Client::new(&second.home).unwrap();
    for client in [&mut a, &mut b] {
        assert!(
            client.tokens(false).await.unwrap()["accessToken"] == "synthetic",
            "Expected fixture credentials"
        );
    }
    // Simulate a slow quota monitor owning the rotation lock. Valid token reads
    // still finish, while refresh requests remain serialized behind that owner.
    let monitoring = service.accounts.lock(account).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(500), a.tokens(false))
            .await
            .unwrap()
            .unwrap()["accessToken"]
            == "synthetic",
        "Expected fixture credentials"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), b.tokens(true))
            .await
            .is_err()
    );
    let mut forged = first.clone();
    forged.home = second.home.clone();
    assert!(
        service
            .accounts
            .access(service, &forged, &json!({ "refresh": false }))
            .await
            .is_err()
    );
    drop(monitoring);
    let (a_refreshed, b_refreshed) = tokio::join!(a.tokens(true), b.tokens(true));
    assert!(
        a_refreshed.as_ref().unwrap()["accessToken"] == "synthetic-refreshed",
        "Expected refreshed fixture credentials"
    );
    assert!(
        a_refreshed.unwrap() == b_refreshed.unwrap(),
        "Fixture refreshes must match"
    );
    // A run can neither roll credentials back nor inject another identity.
    std::fs::write(
        first.home.join("auth.json"),
        json!({ "tokens": { "access_token": "stale", "account_id": "intruder" } }).to_string(),
    )
    .unwrap();
    service.accounts.release(first).await.unwrap();
    assert!(a.tokens(false).await.is_err());
    assert!(
        b.tokens(false).await.unwrap()["accessToken"] == "synthetic-refreshed",
        "Expected refreshed fixture credentials"
    );
}

#[tokio::test]
async fn parallel_runs_share_one_refresh_and_cannot_overwrite_or_reuse_released_credentials() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    service.accounts.initialize(&service).await.unwrap();
    let tokens = json!({
        "access_token": "synthetic",
        "refresh_token": "refresh",
        "account_id": "shared",
    });
    let id = refreshed_account(&service, "Shared", tokens).await;
    let first = acquire(&service, FIRST_RUN, "").await;
    let second = acquire(&service, SECOND_RUN, "").await;
    assert_eq!(first.account_id, second.account_id);
    let _first_broker = broker::serve(&service, &first).await.unwrap();
    let _second_broker = broker::serve(&service, &second).await.unwrap();
    assert_refreshes_are_shared(&service, &id, &first, &second).await;
    assert_eq!(service.accounts.active(&id).await.len(), 1);
    assert!(service.accounts.reconnect(&service, &id).await.is_err());
    assert!(!second.home.join("auth.json").exists());
    let (events, mut receiver) = tokio::sync::mpsc::channel(32);
    let plan = json!({
        "args": [],
        "cwd": root.path(),
        "sandbox": "yolo",
        "output": root.path().join("reply.md"),
        "inputDirectory": root.path(),
        "writableRoots": [],
        "execution": {
            "messageId": "refresh-test",
            "text": "fixture:auth-refresh",
            "attachments": [],
        },
    });
    chat_process::run(
        &service.config,
        &second.home,
        plan,
        events,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    while let Some(event) = receiver.recv().await {
        assert!(!event.to_string().contains("synthetic-refreshed"));
    }
    assert!(root.path().join("reply.md").exists());
    service.accounts.release(&second).await.unwrap();
    assert_eq!(
        service
            .vault
            .get(&format!("codex-account:{id}"))
            .await
            .unwrap()
            .unwrap()["tokens"]["refresh_token"],
        "refresh-rotated-rotated"
    );
}

#[tokio::test]
async fn resident_logs_in_and_releases_managed_account_for_each_native_turn() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    service.accounts.initialize(&service).await.unwrap();
    refreshed_account(
        &service,
        "Resident",
        json!({
            "access_token": "synthetic", "refresh_token": "refresh", "account_id": "resident",
        }),
    )
    .await;
    let lease = acquire(&service, FIRST_RUN, "").await;
    let _broker = broker::serve(&service, &lease).await.unwrap();
    let record = lease.home.join("fixture-lifecycle.jsonl");
    std::fs::write(&record, "").unwrap();
    let socket = root.path().join("resident/codex.sock");
    let endpoint = socket.clone();
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let config = service.config.clone();
    let home = lease.home.clone();
    std::fs::create_dir_all(&config.home).unwrap();
    let native = tokio::spawn(async move {
        chat_process::resident::serve(&config, &home, &endpoint, stopping).await
    });
    eventually(
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || {
            chat_process::resident::ready(&socket)
                .await
                .ok()
                .filter(|ready| *ready)
        },
    )
    .await;
    assert!(
        !std::fs::read_to_string(&record)
            .unwrap()
            .contains("account/login/start"),
        "Anonymous initialization never acquires an account"
    );
    for index in 0..2 {
        let plan = json!({
            "args": [], "codexConfig": { "mcp_servers": {} },
            "cwd": root.path(), "sandbox": "yolo", "reasoning": "low",
            "output": root.path().join("reply.md"), "inputDirectory": root.path(),
            "writableRoots": [],
            "execution": { "messageId": format!("message-{index}"), "text": "fixture:auth-refresh", "attachments": [], },
        });
        let (events, mut received) = tokio::sync::mpsc::channel::<Value>(64);
        let output = tokio::spawn(async move {
            while let Some(event) = received.recv().await {
                assert!(!event.to_string().contains("synthetic"));
            }
        });
        chat_process::resident::run(&socket, plan, events, CancellationToken::new())
            .await
            .unwrap();
        output.await.unwrap();
        assert!(!lease.home.join("auth.json").exists());
    }
    stop.cancel();
    native.await.unwrap().unwrap();
    service.accounts.release(&lease).await.unwrap();
    let log: Vec<Value> = std::fs::read_to_string(&record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for (method, expected) in [
        ("initialize", 1),
        ("account/login/start", 2),
        ("account/logout", 2),
        ("thread/unsubscribe", 2),
    ] {
        assert_eq!(
            log.iter().filter(|entry| entry["method"] == method).count(),
            expected,
            "{method}"
        );
    }
}

#[tokio::test]
async fn parallel_capacity_prefers_usage_and_lowering_limits_does_not_stop_runs() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    service.accounts.initialize(&service).await.unwrap();
    let mut ids = Vec::new();
    for name in ["more", "less"] {
        let tokens = json!({ "access_token": "synthetic", "account_id": name });
        let id = refreshed_account(&service, name, tokens).await;
        if name == "less" {
            let mut account = service.accounts.get(&service, &id).await.unwrap();
            account["usage"]["windows"][0]["usedPercent"] = 90.into();
            service.store.put(KIND, account).await.unwrap();
        }
        ids.push(id);
    }
    let first = acquire(&service, FIRST_RUN, "").await;
    let second = acquire(&service, SECOND_RUN, "").await;
    assert_eq!(first.account_id, ids[0]);
    assert_eq!(second.account_id, ids[0]);
    let limit = |runs: Value| json!({ "maxConcurrentRuns": runs });
    let view = service
        .accounts
        .update(&service, &ids[0], &limit(json!(1)))
        .await
        .unwrap();
    assert_eq!(view["activeRunIds"].as_array().unwrap().len(), 2);
    let third = acquire(&service, "33333333-3333-4333-8333-333333333333", "").await;
    assert_eq!(third.account_id, ids[1]);
    for invalid in [json!(0), json!(-1), json!(1.5), json!("2"), Value::Null] {
        assert!(
            service
                .accounts
                .update(&service, &ids[0], &limit(invalid))
                .await
                .is_err()
        );
    }
    for lease in [first, second, third] {
        service.accounts.release(&lease).await.unwrap();
    }
    service
        .accounts
        .update(&service, &ids[0], &limit(json!(12)))
        .await
        .unwrap();
    let mut leases = Vec::new();
    for _ in 0..12 {
        let lease = acquire(&service, &id(), "").await;
        assert_eq!(lease.account_id, ids[0]);
        leases.push(lease);
    }
    let overflow = acquire(&service, &id(), "").await;
    assert_eq!(overflow.account_id, ids[1]);
    leases.push(overflow);
    for lease in leases {
        service.accounts.release(&lease).await.unwrap();
    }
}
