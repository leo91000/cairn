use leo_agent_manager::{
    accounts::{
        KIND, broker,
        codex::{Client, normalize},
        usage::{blocked, recovered, remaining},
    },
    config::Config,
    provider::Provider,
    rpc::Session,
    service::Service,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;
fn config(root: &TempDir) -> Config {
    Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().to_owned()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        setup_token: "fixture".into(),
        codex_bin: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/codex.mjs")
            .to_string_lossy()
            .into_owned(),
        claude_bin: "claude".into(),
        gh_bin: "gh".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    }
}

#[tokio::test]
async fn failed_initialization_waits_for_the_codex_process_to_exit() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let home = config.home.join(".codex");
    std::fs::create_dir_all(&home).unwrap();
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
    let home = config.home.join(".codex");
    std::fs::create_dir_all(&home).unwrap();
    let file = home.join("fixture-conversation.json");
    let mut history = json!({"id":"fixture-chat","historyMode":"paginated","turns":[
        {"id":"old","status":"completed","items":[
            {"id":"u","type":"userMessage","clientId":"original","content":[{"type":"text","text":"Old request"}]},
            {"id":"c","type":"commandExecution","aggregatedOutput":"x".repeat(2_100_000)},
            {"id":"s","type":"userMessage","clientId":"steered","content":[{"type":"text","text":"Steer request"}]},
            {"id":"a","type":"agentMessage","text":"Completed once"}
        ]}
    ]});
    // Item pagination recovers a large turn without loading all tool output at once.
    for index in 0..16 {
        history["turns"][0]["items"].as_array_mut().unwrap().insert(1, json!({"id":format!("command-{index}"),"type":"commandExecution","aggregatedOutput":"x".repeat(2_100_000)}));
    }
    for index in 0..101 {
        history["turns"].as_array_mut().unwrap().insert(
            0,
            json!({"id":format!("older-{index}"),"status":"completed","items":[]}),
        );
    }
    std::fs::write(&file, history.to_string()).unwrap();
    for (message_id, expected_turns) in [("steered", 102), ("new-message", 103)] {
        let (events, mut receiver) = tokio::sync::mpsc::channel(64);
        let plan = json!({"args":[],"cwd":root.path(),"model":"fixture","reasoning":"medium","sandbox":"yolo","sessionId":"fixture-chat","output":root.path().join("reply.md"),"inputDirectory":root.path(),"writableRoots":[],"execution":{"messageId":message_id,"text":"New request","attachments":[],"recovery":true}});
        leo_agent_manager::chat_process::run(&config, &home, plan, events, Default::default())
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
        json!({"mode":mode}).to_string(),
    )
    .unwrap();
    let view = service.accounts.reconnect(&service, &id).await.unwrap();
    assert_eq!(view["provider"], "codex");
    (root, service, id)
}
async fn sign_in_until(service: &Service, condition: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let view = service.accounts.sign_in().await;
            if condition(&view) {
                return view;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
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
    let notification =
        tokio::time::timeout(std::time::Duration::from_secs(10), session.incoming.recv())
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
    assert!(view["expiresAt"].as_i64().unwrap() > leo_agent_manager::config::now());
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
    let view = sign_in_until(&service, |v| v["state"] != "pending").await;
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
        let view = sign_in_until(&service, |v| v["state"] != "pending").await;
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
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let view = service.accounts.sign_in().await;
            if view["state"] != "pending" {
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
        sign_in_until(&service, |v| v["state"] != "pending").await["state"],
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
        let account = service
            .accounts
            .create(&service, Provider::Codex, name)
            .await
            .unwrap();
        let id = account["id"].as_str().unwrap().to_owned();
        service
            .vault
            .set(
                &format!("codex-account:{id}"),
                &json!({"tokens":{"access_token":"synthetic","account_id":name}}),
            )
            .await
            .unwrap();
        service.accounts.refresh(&service, &id).await.unwrap();
        ids.push(id);
    }
    let catalog = service.models.list(&service).await.unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 3);
    use leo_agent_manager::models::cached_defaults;
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
    let lease = service
        .accounts
        .acquire(
            &service,
            "11111111-1111-4111-8111-111111111111",
            Provider::Codex,
            "fixture-deep",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.account_id, ids[1]);
    service.accounts.release(&lease).await.unwrap();
    service
        .accounts
        .update(&service, &ids[1], &json!({"enabled":false}))
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
        .request("thread/start", json!({"cwd":root.path()}))
        .await
        .unwrap();
    assert_eq!(started["thread"]["id"], "fixture-chat");
    session.request("turn/start", json!({"threadId":"fixture-chat","input":[{"type":"text","text":"Check the workspace"}],"clientUserMessageId":"message-1"})).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
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
        .request("thread/resume", json!({"threadId":"fixture-chat"}))
        .await
        .unwrap();
    let turns = resumed
        .request(
            "thread/turns/list",
            json!({"threadId":"fixture-chat","limit":100}),
        )
        .await
        .unwrap();
    assert_eq!(turns["data"][0]["status"], "completed");
    assert_eq!(turns["data"][0]["items"][0]["clientId"], "message-1");
    resumed.close().await;
}
#[tokio::test]
async fn queued_reasoning_is_idempotent_editable_and_part_of_the_run_snapshot() {
    use leo_agent_manager::config::id;
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    let chat = service.chat_create(json!({})).await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let message_id = id();
    let mut message =
        json!({"id":message_id,"text":"Review this","model":"fixture-deep","reasoning":"ultra"});
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
    service.chat_tick(&Default::default()).await.unwrap();
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
    let steer = json!({"id":id(),"text":"More detail","mode":"steer","reasoning":"ultra"});
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
    let view = sign_in_until(&service, |v| v["state"] != "pending").await;
    assert_eq!(view["state"], "complete", "{view}");
    let accounts = service.accounts.list(&service).await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["remainingPercent"], 60.0);
    assert_eq!(accounts[0]["provider"], "codex");
    assert_eq!(accounts[0]["status"], "next");
    assert!(accounts[0].get("identity").is_none());
    let lease = service
        .accounts
        .acquire(
            &service,
            "11111111-1111-4111-8111-111111111111",
            Provider::Codex,
            "",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.account_id, id);
    assert!(!lease.home.join("auth.json").exists());
    assert!(lease.home.join("leo-managed-auth").exists());
    assert!(service.accounts.remove(&service, id).await.is_err());
    let second = service
        .accounts
        .acquire(
            &service,
            "22222222-2222-4222-8222-222222222222",
            Provider::Codex,
            "",
        )
        .await
        .unwrap()
        .unwrap();
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
    let before = json!({"ordinaryUsageAllowed":true,"rateLimits":{"primary":{"usedPercent":100,"resetsAt":1},"secondary":{"usedPercent":40}},"rateLimitsByLimitId":{"model":{"limitId":"special","normalModelSlug":"model-x","primary":{"usedPercent":95}}},"rateLimitResetCredits":{"availableCount":2}});
    assert_eq!(remaining(&normalize(&before), ""), Some(0.));
    assert_eq!(normalize(&before)["resets"]["available"], 2);
    let mut after = before.clone();
    after["rateLimits"]["primary"]["resetsAt"] = 9999999999_i64.into();
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

#[tokio::test]
async fn parallel_runs_share_one_refresh_and_cannot_overwrite_or_reuse_released_credentials() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    service.accounts.initialize(&service).await.unwrap();
    let account = service
        .accounts
        .create(&service, Provider::Codex, "Shared")
        .await
        .unwrap();
    let id = account["id"].as_str().unwrap();
    service.vault.set(&format!("codex-account:{id}"), &json!({"tokens":{"access_token":"synthetic","refresh_token":"refresh","account_id":"shared"}})).await.unwrap();
    service.accounts.refresh(&service, id).await.unwrap();
    let first = service
        .accounts
        .acquire(
            &service,
            "11111111-1111-4111-8111-111111111111",
            Provider::Codex,
            "",
        )
        .await
        .unwrap()
        .unwrap();
    let second = service
        .accounts
        .acquire(
            &service,
            "22222222-2222-4222-8222-222222222222",
            Provider::Codex,
            "",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.account_id, second.account_id);
    let _first_broker = broker::serve(&service, &first).await.unwrap();
    let _second_broker = broker::serve(&service, &second).await.unwrap();
    let mut a = Client::new(&first.home).unwrap();
    let mut b = Client::new(&second.home).unwrap();
    assert!(
        a.tokens(false).await.unwrap()["accessToken"] == "synthetic",
        "Expected fixture credentials"
    );
    assert!(
        b.tokens(false).await.unwrap()["accessToken"] == "synthetic",
        "Expected fixture credentials"
    );
    // Simulate a slow quota monitor owning the rotation lock. Valid token reads
    // still finish, while refresh requests remain serialized behind that owner.
    let monitoring = service.accounts.lock(id).await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), a.tokens(false))
            .await
            .unwrap()
            .unwrap()["accessToken"]
            == "synthetic",
        "Expected fixture credentials"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), b.tokens(true))
            .await
            .is_err()
    );
    let mut forged = first.clone();
    forged.home = second.home.clone();
    assert!(
        service
            .accounts
            .access(&service, &forged, &json!({"refresh":false}))
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
        json!({"tokens":{"access_token":"stale","account_id":"intruder"}}).to_string(),
    )
    .unwrap();
    service.accounts.release(&first).await.unwrap();
    assert!(a.tokens(false).await.is_err());
    assert!(
        b.tokens(false).await.unwrap()["accessToken"] == "synthetic-refreshed",
        "Expected refreshed fixture credentials"
    );
    assert_eq!(service.accounts.active(id).await.len(), 1);
    assert!(service.accounts.reconnect(&service, id).await.is_err());
    assert!(!second.home.join("auth.json").exists());
    let (events, mut receiver) = tokio::sync::mpsc::channel(32);
    let plan = json!({"args":[],"cwd":root.path(),"sandbox":"yolo","output":root.path().join("reply.md"),"inputDirectory":root.path(),"writableRoots":[],"execution":{"messageId":"refresh-test","text":"fixture:auth-refresh","attachments":[]}});
    leo_agent_manager::chat_process::run(
        &service.config,
        &second.home,
        plan,
        events,
        Default::default(),
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
async fn parallel_capacity_prefers_usage_and_lowering_limits_does_not_stop_runs() {
    let root = TempDir::new().unwrap();
    let service = Service::new(config(&root)).await.unwrap();
    service.accounts.initialize(&service).await.unwrap();
    let mut ids = Vec::new();
    for name in ["more", "less"] {
        let account = service
            .accounts
            .create(&service, Provider::Codex, name)
            .await
            .unwrap();
        let id = account["id"].as_str().unwrap().to_owned();
        service
            .vault
            .set(
                &format!("codex-account:{id}"),
                &json!({"tokens":{"access_token":"synthetic","account_id":name}}),
            )
            .await
            .unwrap();
        service.accounts.refresh(&service, &id).await.unwrap();
        if name == "less" {
            let mut account = service.accounts.get(&service, &id).await.unwrap();
            account["usage"]["windows"][0]["usedPercent"] = 90.into();
            service.store.put(KIND, account).await.unwrap();
        }
        ids.push(id);
    }
    let first = service
        .accounts
        .acquire(
            &service,
            "11111111-1111-4111-8111-111111111111",
            Provider::Codex,
            "",
        )
        .await
        .unwrap()
        .unwrap();
    let second = service
        .accounts
        .acquire(
            &service,
            "22222222-2222-4222-8222-222222222222",
            Provider::Codex,
            "",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.account_id, ids[0]);
    assert_eq!(second.account_id, ids[0]);
    let view = service
        .accounts
        .update(&service, &ids[0], &json!({"maxConcurrentRuns":1}))
        .await
        .unwrap();
    assert_eq!(view["activeRunIds"].as_array().unwrap().len(), 2);
    let third = service
        .accounts
        .acquire(
            &service,
            "33333333-3333-4333-8333-333333333333",
            Provider::Codex,
            "",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(third.account_id, ids[1]);
    for invalid in [json!(0), json!(-1), json!(1.5), json!("2"), Value::Null] {
        assert!(
            service
                .accounts
                .update(&service, &ids[0], &json!({"maxConcurrentRuns":invalid}))
                .await
                .is_err()
        );
    }
    for lease in [first, second, third] {
        service.accounts.release(&lease).await.unwrap();
    }
    service
        .accounts
        .update(&service, &ids[0], &json!({"maxConcurrentRuns":12}))
        .await
        .unwrap();
    let mut leases = Vec::new();
    for _ in 0..12 {
        let lease = service
            .accounts
            .acquire(
                &service,
                &leo_agent_manager::config::id(),
                Provider::Codex,
                "",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.account_id, ids[0]);
        leases.push(lease);
    }
    let overflow = service
        .accounts
        .acquire(
            &service,
            &leo_agent_manager::config::id(),
            Provider::Codex,
            "",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(overflow.account_id, ids[1]);
    leases.push(overflow);
    for lease in leases {
        service.accounts.release(&lease).await.unwrap();
    }
}
