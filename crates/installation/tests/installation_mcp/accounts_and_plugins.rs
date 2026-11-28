use super::*;

#[tokio::test]
async fn plugin_preparation_cannot_publish_after_instance_or_agent_revocation() {
    use axum::{Json, Router, routing::get};
    use tokio::sync::Notify;

    for operation in ["form", "test", "save"] {
        for revoke_grant in [false, true] {
            let root = TempDir::new().unwrap();
            let s = service(&root).await;
            s.plugins
                .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
            let main = enabled_main(&s).await;
            let (_, configuration) = active_run(&s, &main).await;
            let started = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let app = Router::new().route(
                "/check",
                get({
                    let started = started.clone();
                    let release = release.clone();
                    move || {
                        let started = started.clone();
                        let release = release.clone();
                        async move {
                            started.notify_one();
                            release.notified().await;
                            Json(json!({}))
                        }
                    }
                }),
            );
            let (origin, server) = common::serve_locally(app).await;
            s.plugins.route_http_to(Some(origin));
            let installed = common::package_install(&s,&json!({
                "manifest": {
                    "id": "fenced", "name": "Fenced", "version": "1", "apiVersion": 2,
                    "capabilities": ["http"], "hosts": ["api.example.test"], "agentAccess": {},
                    "instanceForm": true, "instanceTest": true,
                    "settings": [
                        { "key": "check", "title": "Check", "type": "boolean", "default": false },
                        { "key": "apiKey", "title": "API key", "type": "secret", "required": true }
                    ]
                },
                "source": "local function check() cairn.http.request({ url = 'https://api.example.test/check' }) end return { form = function(input) if input.values.check then check() end return {} end, test = function() check(); return { ok = true, message = 'Checked' } end }"
            })).await.unwrap();
            common::package_approve(&s, "fenced", "1", text(&installed, "digest"))
                .await
                .unwrap();
            let initial = json!({ "name": "Initial", "enabled": true, "settings": { "check": false }, "secrets": { "apiKey": "synthetic-one" }, "agentIds": [MAIN_AGENT_ID] });
            let saved = common::save_plugin(&s, "fenced", &initial, None)
                .await
                .unwrap();
            let instance = saved["id"].as_str().unwrap();
            let key = cairn_installation::plugins::connections::secrets_key(instance);
            let secret_before = s.vault.get(&key).await.unwrap();
            let mut args =
                json!({ "kind": operation, "pluginId": "fenced", "instanceId": instance });
            if operation == "form" {
                args["data"] = json!({ "settings": { "check": true }, "secrets": {}, "revision": saved["revision"] });
            }
            if operation == "save" {
                args["data"] = json!({
                    "name": "Stale edit",
                    "enabled": true,
                    "settings": { "check": true },
                    "secrets": { "apiKey": "synthetic-two" },
                    "revision": saved["revision"],
                });
            }
            let revoke = async {
                started.notified().await;
                if revoke_grant {
                    s.agent_fixture(json!({ "plugins": [] }), Some(MAIN_AGENT_ID))
                        .await
                        .unwrap();
                } else {
                    let mut edit = initial.clone();
                    edit.as_object_mut().unwrap().remove("agentIds");
                    edit["name"] = json!("Owner edit");
                    common::save_plugin(&s, "fenced", &edit, Some(instance))
                        .await
                        .unwrap();
                }
                release.notify_one();
            };
            let (response, ()) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                tokio::join!(
                    Box::pin(call(&s, &configuration, "manage_plugin", args)),
                    Box::pin(revoke)
                )
            })
            .await
            .unwrap();
            assert_eq!(
                response["result"]["isError"], true,
                "{operation}, grant={revoke_grant}: {response}"
            );
            let current = s.store.plugin_instance(instance).await.unwrap().unwrap();
            assert_ne!(current.name, "Stale edit");
            assert_eq!(current.settings["check"], false);
            assert_eq!(
                s.vault.get(&key).await.unwrap(),
                secret_before,
                "A refused save must not rotate the secret"
            );
            server.abort();
        }
    }
}

#[tokio::test]
async fn package_and_program_capabilities_share_discovery_scopes_and_owner_refusals() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    for internal in [false, true] {
        let catalog = if internal {
            call(&s, &configuration, "list_plugins", json!({})).await
        } else {
            public_call(&s, &["read"], "list_plugins", json!({})).await
        };
        let mut ids = result(&catalog)["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, vec!["discord", "github", "gmail", "onepassword"]);
        let programs = if internal {
            call(
                &s,
                &configuration,
                "list_plugin_programs",
                json!({"pluginId":"onepassword"}),
            )
            .await
        } else {
            public_call(
                &s,
                &["read"],
                "list_plugin_programs",
                json!({"pluginId":"onepassword"}),
            )
            .await
        };
        assert_eq!(result(&programs)["programs"][0]["name"], "op");
        for (name, input) in [
            (
                "approve_plugin_package",
                json!({"pluginId":"fixture","version":"1","digest":"a".repeat(64)}),
            ),
            (
                "prepare_plugin_program",
                json!({"kind":"install","pluginId":"onepassword","program":"op","digest":"a".repeat(64)}),
            ),
        ] {
            let refused = if internal {
                call(&s, &configuration, name, input).await
            } else {
                public_call(&s, &["manage"], name, input).await
            };
            assert_eq!(refused["result"]["isError"], true, "{refused}");
            assert!(refused.to_string().contains("owner"), "{refused}");
        }
    }
    let refused = public_call(
        &s,
        &["read"],
        "install_plugin_package",
        json!({"manifest":{},"source":"return {}","modules":{}}),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true);
    assert!(refused.to_string().contains("manage"));
}

#[tokio::test]
async fn expired_installation_authorization_cannot_publish_a_prepared_package() {
    use std::os::unix::fs::PermissionsExt;
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let started = root.path().join("worker-started");
    let resume = root.path().join("worker-resume");
    let wrapper = root.path().join("plugin-worker-gate");
    let quote =
        |path: &std::path::Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    std::fs::write(&wrapper,format!("#!/bin/sh\n: > {}\ni=0\nwhile [ ! -f {} ] && [ $i -lt 500 ]; do sleep 0.01; i=$((i+1)); done\nexec {} \"$@\"\n",quote(&started),quote(&resume),quote(std::path::Path::new(env!("CARGO_BIN_EXE_cairn"))))).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    s.plugins.use_worker(wrapper);
    let task = tokio::spawn({
        let s = s.clone();
        async move {
            call(&s,&configuration,"install_plugin_package",json!({"manifest":{"id":"expired-upload","name":"Expired","version":"1","apiVersion":2,"capabilities":[]},"source":"return {}","modules":{}})).await
        }
    });
    common::eventually(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(10),
        async || started.exists().then_some(()),
    )
    .await;
    s.store
        .change_run(
            run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    std::fs::write(&resume, "").unwrap();
    let refused = task.await.unwrap();
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(!root.path().join("data/plugins/expired-upload").exists());
    assert!(
        s.store
            .read(|db| db.plugin_package("expired-upload"))
            .await
            .unwrap()
            .revision
            .is_none()
    );
}

#[tokio::test]
async fn quota_coding_agent_switch_has_equivalent_public_and_builtin_tools() {
    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (_, configuration) = active_run(&s, &main).await;
        let invoke = |name, args| {
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                if internal {
                    call(&s, &configuration, name, args).await
                } else {
                    public_call(&s, &["read", "manage", "run"], name, args).await
                }
            }
        };
        let created = invoke("create_chat", json!({})).await;
        let chat_id = result(&created)["id"].as_str().unwrap();
        let message_id = uuid::Uuid::new_v4().to_string();
        let sent = invoke(
            "send_chat_message",
            json!({"chatId":chat_id,"message":{"id":message_id,"text":"Keep this request"}}),
        )
        .await;
        result(&sent);
        s.chat_tick(&std::collections::HashSet::new())
            .await
            .unwrap();
        let detail = s.chat_detail(chat_id).await.unwrap();
        let run_id = text(&detail, "runId");
        s.store
            .change_run(
                run_id,
                cairn_installation::runs::RunChanges {
                    account_wait_reason: Some(Some(
                        "Waiting for a Codex account with available usage.".into(),
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut account = serde_json::to_value(
            s.accounts
                .create(
                    &s,
                    cairn_installation::provider::Provider::Claude,
                    "Alternative",
                )
                .await
                .unwrap(),
        )
        .unwrap();
        account["state"] = "ready".into();
        s.store.save_account_fixture(account).await.unwrap();
        let options = invoke("get_chat_coding_agent_options", json!({"chatId":chat_id})).await;
        assert_eq!(
            result(&options)["choices"],
            json!([{"provider":"claude","label":"Claude Code"}])
        );
        let input = json!({"chatId":chat_id,"runId":run_id,"turnId":message_id,"requestId":uuid::Uuid::new_v4(),"provider":"claude"});
        let denied = public_call(&s, &["read"], "switch_chat_coding_agent", input.clone()).await;
        assert_eq!(denied["result"]["isError"], true);
        let switched = invoke("switch_chat_coding_agent", input.clone()).await;
        assert_eq!(result(&switched)["codingAgentSwitch"]["provider"], "claude");
        let retried = invoke("switch_chat_coding_agent", input).await;
        assert_eq!(result(&retried)["codingAgentSwitch"]["provider"], "claude");
        let stopped = invoke("stop_chat", json!({"chatId":chat_id})).await;
        result(&stopped);
        assert!(s.chat_detail(chat_id).await.unwrap()["codingAgentSwitch"].is_null());
    }
}

#[tokio::test]
async fn typed_account_management_has_tool_parity_without_human_sign_in_authority() {
    use cairn_installation::{
        accounts::{Account, AccountState},
        provider::Provider,
    };
    let root = TempDir::new().unwrap();
    let mut s = service(&root).await;
    common::reconfigure(&mut s, |config| {
        config.antigravity_bin = common::fixture("antigravity.mjs");
    })
    .await;
    let mut google = Account::new(Provider::Antigravity, "Google", AccountState::Ready);
    google.model_provider = Some("google".into());
    s.store.save_coding_account(&google).await.unwrap();
    s.vault
        .set(
            &format!("antigravity-account:{}", google.id),
            &json!({
                "kind": "google",
                "token": {
                    "token": {
                        "access_token": "fixture-google-quota-access",
                        "expiry": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                    },
                },
            }),
        )
        .await
        .unwrap();
    let refreshed = common::owner_api_route(&s, "POST", "/api/accounts/refresh", json!({}))
        .await
        .unwrap();
    assert_eq!(refreshed["kind"], "refreshed");
    common::eventually(
        std::time::Duration::from_secs(10),
        std::time::Duration::from_millis(30),
        async || {
            s.accounts
                .list(&s)
                .await
                .unwrap()
                .into_iter()
                .find(|account| account.id == google.id)
                .and_then(|account| account.antigravity_quota)
        },
    )
    .await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let mut account = Account::new(Provider::Codex, "Private", AccountState::Ready);
    account.identity = Some("synthetic-private-fingerprint".into());
    s.store.save_coding_account(&account).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (status, response) = node_management_call(
            &app,
            endpoint,
            token,
            "list_coding_agent_accounts",
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let listed = result(&response);
        let accounts = listed["accounts"].as_array().unwrap();
        let codex = accounts
            .iter()
            .find(|item| item["id"] == account.id)
            .unwrap();
        assert!(codex.get("identity").is_none());
        let antigravity = accounts
            .iter()
            .find(|item| item["id"] == google.id)
            .unwrap();
        assert!(antigravity["usage"].is_null());
        assert_eq!(
            antigravity["antigravityQuota"]["windows"][0]["remainingPercent"].as_f64(),
            Some(75.0),
        );
        assert!(listed["signIn"].is_null());
        let current = s.accounts.account(&s, &account.id).await.unwrap();
        let (_,response)=node_management_call(&app,endpoint,token,"replace_coding_agent_account",json!({"accountId":account.id,"revision":current.revision,"settings":{"name":"Managed","enabled":false,"maxConcurrentRuns":2}})).await;
        assert_eq!(result(&response)["name"], "Managed");
        let unchanged = s.accounts.account(&s, &account.id).await.unwrap();
        let (_, refused) = node_management_call(
            &app,
            endpoint,
            token,
            "refresh_coding_agent_accounts",
            json!({}),
        )
        .await;
        assert_eq!(result(&refused)["kind"], "humanRequired");
        assert_eq!(result(&refused)["path"], "/connections");
        assert_eq!(
            s.accounts.account(&s, &account.id).await.unwrap(),
            unchanged
        );
        assert_eq!(
            s.accounts.account(&s, &google.id).await.unwrap().usage,
            google.usage,
        );
        let before = s.store.coding_accounts().await.unwrap().len();
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "connect_coding_agent_account",
            json!({"provider":"codex","name":"Human flow"}),
        )
        .await;
        assert_eq!(result(&response)["kind"], "humanRequired");
        assert_eq!(s.store.coding_accounts().await.unwrap().len(), before);
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "delete_coding_agent_account",
            json!({"accountId":account.id}),
        )
        .await;
        assert_eq!(result(&response)["kind"], "humanRequired");
        assert!(s.store.coding_account(&account.id).await.unwrap().is_some());
    }
    let current = s.accounts.account(&s, &account.id).await.unwrap();
    let (_,denied)=node_management_call(&app,"/mcp",&read_token,"replace_coding_agent_account",json!({"accountId":account.id,"revision":current.revision,"settings":{"name":"Forbidden","enabled":true,"maxConcurrentRuns":1}})).await;
    assert_eq!(denied["result"]["isError"], true);
    assert_eq!(s.accounts.account(&s, &account.id).await.unwrap(), current);
    s.store
        .delete(&format!(
            "test-relay:{}",
            cairn_installation::auth::digest(&owner_token)
        ))
        .await
        .unwrap();
    let (_, revoked) = node_management_call(
        &app,
        "/mcp",
        &owner_token,
        "list_coding_agent_accounts",
        json!({}),
    )
    .await;
    assert!(
        revoked.get("error").is_some() || revoked["result"]["isError"] == true,
        "{revoked}"
    );
}

#[tokio::test]
async fn model_catalogs_and_unsupported_account_capabilities_share_all_three_transports() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let opencode = root.path().join("opencode-fixture");
    std::os::unix::fs::symlink(common::fixture_path("opencode.mjs"), &opencode).unwrap();
    let antigravity = root.path().join("antigravity-fixture");
    std::os::unix::fs::symlink(common::fixture_path("antigravity.mjs"), &antigravity).unwrap();
    let s = Service::new(cairn_installation::config::Config {
        codex_bin: common::fixture("codex.mjs"),
        claude_bin: common::fixture("claude.mjs"),
        opencode_bin: opencode.to_string_lossy().into_owned(),
        antigravity_bin: antigravity.to_string_lossy().into_owned(),
        ..common::config(root.path())
    })
    .await
    .unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let public = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);
    let member =
        common::RelayContext::new(&common::relay_fixture::member_context(&s, "member").await);
    for provider in ["codex", "claude", "opencode"] {
        for (tool, path) in [
            ("list_models", format!("/api/{provider}/models")),
            (
                "list_coding_agent_account_providers",
                format!("/api/accounts/providers/{provider}"),
            ),
        ] {
            let (status, expected) = account_http(&app, &owner, "GET", &path, Value::Null).await;
            assert_eq!(status, StatusCode::OK, "{expected}");
            for (endpoint, token) in [
                ("/mcp", public.as_str()),
                ("/mcp-installation", agent_token),
            ] {
                let (_, response) =
                    node_management_call(&app, endpoint, token, tool, json!({"provider":provider}))
                        .await;
                assert_eq!(result(&response), &expected, "{tool} {provider}");
            }
            if tool == "list_models" {
                let _: cairn_contracts::ModelCatalog =
                    serde_json::from_value(expected.clone()).unwrap();
                assert!(expected.get("source").is_none());
                if provider == "codex" {
                    assert_eq!(expected["connected"], false, "{expected}");
                }
            } else if provider != "opencode" {
                assert_eq!(expected["kind"], "unsupported");
                assert_eq!(expected["capability"], "apiKey");
            }
        }
    }
    let codex_home = s.config.home.join(".codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let host_auth = codex_home.join("auth.json");
    std::fs::write(
        &host_auth,
        br#"{"tokens":{"access_token":"synthetic-host-login"}}"#,
    )
    .unwrap();
    let (status, codex) =
        account_http(&app, &member, "GET", "/api/codex/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{codex}");
    assert_eq!(codex["connected"], true, "{codex}");
    assert!(!codex.to_string().contains("synthetic-host-login"));
    std::fs::remove_file(host_auth).unwrap();

    let (status, _) = account_http(&app, &member, "GET", "/api/accounts", Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, unsupported) = account_http(
        &app,
        &owner,
        "POST",
        "/api/accounts",
        json!({"provider":"codex","name":"Invalid","apiKey":"synthetic-key"}),
    )
    .await;
    assert_eq!(unsupported["kind"], "unsupported");
    assert!(s.store.coding_accounts().await.unwrap().is_empty());
    let mut ids = Vec::new();
    for (model_provider, key) in [
        ("opencode", "synthetic-zen-key"),
        ("openai", "synthetic-openai-key"),
    ] {
        let (status, connected) = account_http(&app, &owner, "POST", "/api/accounts", json!({
            "provider":"opencode", "name":model_provider, "modelProvider":model_provider, "apiKey":key
        })).await;
        assert_eq!(status, StatusCode::OK, "{connected}");
        assert_eq!(connected["kind"], "connected");
        assert!(!connected.to_string().contains(key));
        ids.push(text(&connected["account"], "id").to_owned());
    }
    let (status, refreshed) =
        account_http(&app, &owner, "POST", "/api/accounts/refresh", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["kind"], "refreshed");
    let (_, replaced) = account_http(
        &app,
        &owner,
        "POST",
        &format!("/api/accounts/{}/api-key", ids[0]),
        json!({"modelProvider":"opencode","apiKey":"synthetic-replacement-key"}),
    )
    .await;
    assert_eq!(replaced["kind"], "connected");
    assert!(!replaced.to_string().contains("synthetic-replacement-key"));
    let (_, unavailable) = account_http(
        &app,
        &owner,
        "POST",
        &format!("/api/accounts/{}/sign-in", ids[0]),
        json!({}),
    )
    .await;
    assert_eq!(unavailable["kind"], "unsupported");
    assert_eq!(unavailable["capability"], "signIn");
    ids.reverse();
    let (_, ordered) = account_http(
        &app,
        &owner,
        "POST",
        "/api/accounts/order",
        json!({"provider":"opencode","ids":ids}),
    )
    .await;
    assert_eq!(ordered["kind"], "ordered");
    for (endpoint, token) in [
        ("/mcp", public.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "order_coding_agent_accounts",
            json!({"provider":"opencode","ids":ids}),
        )
        .await;
        assert_eq!(result(&response), &ordered);
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "order_coding_agent_accounts",
            json!({"provider":"codex","ids":[]}),
        )
        .await;
        assert_eq!(result(&response)["kind"], "unsupported");
        assert_eq!(result(&response)["capability"], "order");
        let (status, expected) =
            account_http(&app, &owner, "GET", "/api/opencode/models", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{expected}");
        assert!(!expected["models"].as_array().unwrap().is_empty());
        assert_eq!(expected["connected"], true, "{expected}");
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "list_models",
            json!({"provider":"opencode"}),
        )
        .await;
        assert_eq!(result(&response), &expected);

        // An agent without an account says so, for clients to point to Connections.
        let (status, unconnected) =
            account_http(&app, &owner, "GET", "/api/antigravity/models", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{unconnected}");
        assert_eq!(unconnected["connected"], false, "{unconnected}");
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "list_models",
            json!({"provider":"antigravity"}),
        )
        .await;
        assert_eq!(result(&response), &unconnected);
    }

    // A changed pool invalidates cached inventory. Failure to launch discovery does not erase
    // the other enabled account, and callers can still select a pinned model.
    let mut paused = s.accounts.account(&s, &ids[0]).await.unwrap();
    paused.enabled = false;
    s.store.save_coding_account(&paused).await.unwrap();
    std::fs::remove_file(opencode).unwrap();
    let (status, stale) =
        account_http(&app, &member, "GET", "/api/opencode/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{stale}");
    assert_eq!(stale["connected"], true, "{stale}");
    assert_eq!(stale["stale"], true, "{stale}");
    assert!(!text(&stale, "error").is_empty(), "{stale}");

    let (status, available) = account_http(
        &app,
        &member,
        "GET",
        "/api/opencode/models?availabilityOnly=true",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{available}");
    assert_eq!(available["connected"], true, "{available}");
    assert_eq!(available["models"], json!([]), "{available}");
    assert_eq!(available["error"], "", "{available}");
    let (status, availability_route) = account_http(
        &app,
        &member,
        "GET",
        "/api/opencode/model-availability",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{availability_route}");
    assert_eq!(availability_route, available);

    let mut google = cairn_installation::accounts::Account::new(
        cairn_installation::provider::Provider::Antigravity,
        "Expired fixture login",
        cairn_installation::accounts::AccountState::Ready,
    );
    google.model_provider = Some("google".into());
    s.store.save_coding_account(&google).await.unwrap();
    s.vault
        .set(
            &format!("antigravity-account:{}", google.id),
            &json!({
                "kind": "google",
                "token": {
                    "token": {
                        "access_token": "synthetic-expired-google-token",
                        "expiry": "2000-01-01T00:00:00Z",
                    },
                },
            }),
        )
        .await
        .unwrap();
    let (status, reconnect) =
        account_http(&app, &member, "GET", "/api/antigravity/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reconnect}");
    assert_eq!(reconnect["connected"], false, "{reconnect}");
    let (status, connections) =
        account_http(&app, &owner, "GET", "/api/accounts", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let expired = connections["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["id"] == google.id)
        .unwrap();
    assert_eq!(expired["status"], "reconnect", "{expired}");
    assert!(text(expired, "error").contains("Reconnect this Antigravity Google account"));
    assert!(
        !reconnect
            .to_string()
            .contains("synthetic-expired-google-token")
    );

    for (endpoint, token) in [
        ("/mcp", public.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "list_models",
            json!({
                "provider": "opencode",
                "availabilityOnly": true,
            }),
        )
        .await;
        assert_eq!(result(&response), &available);
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "list_models",
            json!({"provider":"opencode"}),
        )
        .await;
        assert_eq!(result(&response), &stale);
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "list_models",
            json!({"provider":"antigravity"}),
        )
        .await;
        assert_eq!(result(&response), &reconnect);
        let (_, invalid) = node_management_call(
            &app,
            endpoint,
            token,
            "list_models",
            json!({"provider":"unknown"}),
        )
        .await;
        assert_eq!(invalid["result"]["isError"], true);
    }

    let (status, gemini) = account_http(
        &app,
        &owner,
        "POST",
        "/api/accounts",
        json!({
            "provider": "antigravity",
            "name": "Gemini fixture key",
            "modelProvider": "gemini",
            "apiKey": "synthetic-gemini-fixture-key",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{gemini}");
    assert_eq!(gemini["kind"], "connected", "{gemini}");
    let gemini_id = text(&gemini["account"], "id");
    std::fs::remove_file(antigravity).unwrap();

    // When Gemini inventory is temporarily unavailable, order must not turn a separate
    // expired login into an installation-wide reconnection requirement.
    let google_id = google.id.clone();
    for ordered_accounts in [[gemini_id, &google_id], [&google_id, gemini_id]] {
        google.mark_ready();
        s.store.save_coding_account(&google).await.unwrap();
        let (status, ordered) = account_http(
            &app,
            &owner,
            "POST",
            "/api/accounts/order",
            json!({
                "provider": "antigravity",
                "ids": ordered_accounts,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ordered}");
        assert_eq!(ordered["kind"], "ordered", "{ordered}");

        let (status, mixed) =
            account_http(&app, &member, "GET", "/api/antigravity/models", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{mixed}");
        assert_eq!(mixed["connected"], true, "{mixed}");
        assert_eq!(mixed["stale"], true, "{mixed}");
        for (endpoint, token) in [
            ("/mcp", public.as_str()),
            ("/mcp-installation", agent_token),
        ] {
            let (_, response) = node_management_call(
                &app,
                endpoint,
                token,
                "list_models",
                json!({"provider":"antigravity"}),
            )
            .await;
            assert_eq!(result(&response), &mixed);
        }
    }

    // A vanished executable is transient even for an expired Google access token if
    // the refresh credential has not been rejected.
    let mut gemini = s.accounts.account(&s, gemini_id).await.unwrap();
    gemini.enabled = false;
    s.store.save_coding_account(&gemini).await.unwrap();
    google.mark_ready();
    s.store.save_coding_account(&google).await.unwrap();
    let google_secret = format!("antigravity-account:{}", google.id);
    let mut credentials = s.vault.get(&google_secret).await.unwrap().unwrap();
    credentials["token"]["token"]["refresh_token"] = "synthetic-google-refresh-token".into();
    s.vault.set(&google_secret, &credentials).await.unwrap();
    let (status, failed_renewal) =
        account_http(&app, &member, "GET", "/api/antigravity/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{failed_renewal}");
    assert_eq!(failed_renewal["connected"], true, "{failed_renewal}");
    assert_eq!(failed_renewal["stale"], true, "{failed_renewal}");
    s.accounts.refresh(&s, &google.id).await.unwrap();
    assert!(
        s.accounts
            .account(&s, &google.id)
            .await
            .unwrap()
            .is_active()
    );
    let (_, after_poll) =
        account_http(&app, &member, "GET", "/api/antigravity/models", Value::Null).await;
    assert_eq!(after_poll, failed_renewal);

    let temporary_accounts = s.config.data_dir.join("antigravity-accounts");
    std::fs::create_dir_all(&temporary_accounts).unwrap();
    std::fs::write(
        temporary_accounts.join(&google.id),
        b"fixture blocking staging",
    )
    .unwrap();
    s.accounts.refresh(&s, &google.id).await.unwrap();
    assert!(
        s.accounts
            .account(&s, &google.id)
            .await
            .unwrap()
            .is_active()
    );
}

#[tokio::test]
async fn agents_follow_owner_sign_in_without_challenge_or_mutation_authority() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let s = Service::new(cairn_installation::config::Config {
        codex_bin: common::fixture("codex.mjs"),
        claude_bin: common::fixture("claude.mjs"),
        opencode_bin: common::fixture("opencode.mjs"),
        ..common::config(root.path())
    })
    .await
    .unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let public = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);
    let (status, started) = account_http(
        &app,
        &owner,
        "POST",
        "/api/accounts",
        json!({"provider":"claude","name":"Owner flow"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let id = text(&started["signIn"], "accountId");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if s.accounts
                .sign_in()
                .await
                .is_some_and(|sign_in| sign_in.accepts_code)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let before = s.accounts.account(&s, id).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", public.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, progress) = node_management_call(
            &app,
            endpoint,
            token,
            "get_coding_agent_account_sign_in",
            json!({}),
        )
        .await;
        let progress = result(&progress);
        assert_eq!(
            progress["signIn"],
            json!({"accountId":id,"provider":"claude","state":"pending","phase":"authorizing"})
        );
        let (status, http) =
            account_http(&app, &owner, "GET", "/api/accounts/sign-in", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(progress, &http);
        for (name, input) in [
            (
                "connect_coding_agent_account",
                json!({"provider":"claude","name":"Forbidden"}),
            ),
            ("reconnect_coding_agent_account", json!({"accountId":id})),
            (
                "replace_coding_agent_account_key",
                json!({"accountId":id,"modelProvider":"openai","apiKey":"synthetic-key"}),
            ),
            (
                "submit_coding_agent_account_code",
                json!({"code":"fixture-code"}),
            ),
            ("cancel_coding_agent_account_sign_in", json!({})),
            ("delete_coding_agent_account", json!({"accountId":id})),
            ("refresh_coding_agent_accounts", json!({})),
        ] {
            let (_, response) = node_management_call(&app, endpoint, token, name, input).await;
            assert_eq!(result(&response)["kind"], "humanRequired", "{name}");
            assert_eq!(result(&response)["path"], "/connections");
        }
        assert_eq!(s.accounts.account(&s, id).await.unwrap(), before);
        assert!(s.accounts.sign_in().await.unwrap().accepts_code);
    }
    let (_, submitted) = account_http(
        &app,
        &owner,
        "POST",
        "/api/accounts/sign-in/code",
        json!({"code":"fixture-code"}),
    )
    .await;
    assert_eq!(submitted["kind"], "completed");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if s.accounts
                .sign_in()
                .await
                .is_some_and(|sign_in| sign_in.state == cairn_contracts::SignInState::Complete)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (status, catalog) =
        account_http(&app, &owner, "GET", "/api/claude/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert!(!catalog["models"].as_array().unwrap().is_empty());
    assert!(catalog.get("source").is_none());
    for (endpoint, token) in [
        ("/mcp", public.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "list_models",
            json!({"provider":"claude"}),
        )
        .await;
        assert_eq!(result(&response), &catalog);
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "get_coding_agent_account_sign_in",
            json!({}),
        )
        .await;
        assert_eq!(result(&response)["signIn"]["state"], "complete");
        assert!(result(&response)["signIn"].get("url").is_none());
        assert!(result(&response)["signIn"].get("code").is_none());
    }
    let (_, reconnect) = account_http(
        &app,
        &owner,
        "POST",
        &format!("/api/accounts/{id}/sign-in"),
        json!({}),
    )
    .await;
    assert_eq!(reconnect["kind"], "signIn");
    let (_, cancelled) =
        account_http(&app, &owner, "DELETE", "/api/accounts/sign-in", Value::Null).await;
    assert_eq!(cancelled["kind"], "completed");
    let (_, deleted) = account_http(
        &app,
        &owner,
        "DELETE",
        &format!("/api/accounts/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(deleted["kind"], "completed");
    assert!(s.store.coding_account(id).await.unwrap().is_none());
}

#[tokio::test]
async fn eventless_side_question_stays_on_its_reply_after_item_id_reuse() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let claude = s
        .agent_fixture(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    let (run, _) = active_run(&s, &claude).await;
    let run_id = run["id"].as_str().unwrap();
    let chat_id = text(&run, "taskId").to_owned();
    s.side_questions_available(run_id, true).await.unwrap();
    let item = |text: &str| {
        json!({
            "item": {
                "id": "reused",
                "type": "agent_message",
                "text": text,
            },
        })
    };
    s.store
        .event(
            run_id,
            &EventKind::ItemCompleted,
            "First",
            Some(item("First")),
        )
        .await
        .unwrap();
    let request = json!({
        "chatId": chat_id,
        "question": {
            "id": uuid::Uuid::new_v4(),
            "text": "Why?",
            "itemId": "reused",
        },
    });
    let asked = public_call(&s, &["run"], "ask_chat_side_question", request.clone()).await;
    let saved = result(&asked).clone();
    assert!(
        !saved["replyEventId"].as_str().unwrap().is_empty(),
        "{asked}"
    );
    s.store
        .event(run_id, &EventKind::TurnCompleted, "", None)
        .await
        .unwrap();
    s.store
        .event(
            run_id,
            &EventKind::ItemCompleted,
            "Second",
            Some(item("Second")),
        )
        .await
        .unwrap();
    let retry = call(&s, &configuration, "ask_chat_side_question", request).await;
    assert_eq!(result(&retry)["id"], saved["id"], "{retry}");
    assert_eq!(
        result(&retry)["replyEventId"],
        saved["replyEventId"],
        "{retry}"
    );
}

#[tokio::test]
async fn oversized_numeric_reply_identities_are_refused_on_http_and_both_tools() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let claude = s
        .agent_fixture(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    let (run, _) = active_run(&s, &claude).await;
    let run_id = run["id"].as_str().unwrap();
    let chat_id = text(&run, "taskId").to_owned();
    s.side_questions_available(run_id, true).await.unwrap();
    s.store
        .event(
            run_id,
            &EventKind::ItemCompleted,
            "First",
            Some(json!({
                "item": {
                    "id": "reply-1",
                    "type": "agent_message",
                    "text": "First",
                },
            })),
        )
        .await
        .unwrap();
    let valid = public_call(
        &s,
        &["run"],
        "ask_chat_side_question",
        json!({
            "chatId": chat_id,
            "question": {
                "id": uuid::Uuid::new_v4(),
                "text": "Why?",
                "itemId": "reply-1",
            },
        }),
    )
    .await;
    let event_id = result(&valid)["replyEventId"].as_str().unwrap();
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);
    for padded in [
        format!("{event_id:0>17}"),
        format!("{}{event_id}", "0".repeat(300_000)),
    ] {
        let question = || {
            json!({
                "id": uuid::Uuid::new_v4(),
                "text": "Why?",
                "itemId": "reply-1",
                "replyEventId": padded,
            })
        };
        // The short overlength case reaches validation within the HTTP body cap.
        // The public MCP also admits the larger request at its transport boundary.
        if padded.len() == 17 {
            let request = owner
                .authorize(common::request(
                    "POST",
                    &format!("/api/chats/{chat_id}/side-questions"),
                ))
                .body(Body::from(question().to_string()))
                .unwrap();
            let response = common::send(&app, request).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(
                common::read_json(response)
                    .await
                    .to_string()
                    .contains("too long")
            );
        }
        for response in [
            public_call(
                &s,
                &["run"],
                "ask_chat_side_question",
                json!({ "chatId": chat_id, "question": question() }),
            )
            .await,
            call(
                &s,
                &configuration,
                "ask_chat_side_question",
                json!({ "chatId": chat_id, "question": question() }),
            )
            .await,
        ] {
            assert_eq!(response["result"]["isError"], true);
            assert!(response.to_string().contains("too long"));
        }
    }
    let listed = public_call(
        &s,
        &["read"],
        "list_chat_side_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(
        result(&listed)["sideQuestions"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn pruned_side_question_ids_cannot_be_replayed_or_reused() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let claude = s
        .agent_fixture(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    let (run, _) = active_run(&s, &claude).await;
    let run_id = run["id"].as_str().unwrap();
    let chat_id = text(&run, "taskId").to_owned();
    s.side_questions_available(run_id, true).await.unwrap();
    let old_id = uuid::Uuid::new_v4().to_string();
    for index in 0..31 {
        let question_id = if index == 0 {
            old_id.clone()
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        s.store
            .set(
                &format!("chat-side-question:{chat_id}:{question_id}"),
                json!({
                    "id": question_id, "chatId": chat_id, "runId": run_id, "text": "Old question",
                    "quote": "", "itemId": "", "replyEventId": "", "provider": "claude",
                    "status": "answered", "answer": "Done", "error": null,
                    "createdAt": index, "settledAt": index + 1,
                }),
                None,
            )
            .await
            .unwrap();
    }
    let fresh = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        json!({
            "chatId": chat_id, "question": { "id": uuid::Uuid::new_v4(), "text": "Fresh" },
        }),
    )
    .await;
    assert_ne!(fresh["result"]["isError"], true, "{fresh}");
    for text in ["Old question", "Changed question"] {
        let retry = call(
            &s,
            &configuration,
            "ask_chat_side_question",
            json!({
                "chatId": chat_id, "question": { "id": old_id, "text": text },
            }),
        )
        .await;
        assert_eq!(retry["result"]["isError"], true, "{retry}");
        assert!(retry.to_string().contains("no longer retained"), "{retry}");
    }
}

#[tokio::test]
async fn automatic_compaction_closes_side_questions_on_http_and_both_mcp_surfaces() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let claude = s
        .agent_fixture(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    let (target, _) = active_run(&s, &claude).await;
    let run_id = target["id"].as_str().unwrap();
    let chat_id = text(&target, "taskId").to_owned();
    s.side_questions_available(run_id, true).await.unwrap();
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);

    for compacting in [true, false] {
        s.store
            .update_run_boundary(run_id, json!({ "sideQuestionsCompacting": compacting }))
            .await
            .unwrap();
        let read = owner
            .authorize(common::request("GET", &format!("/api/chats/{chat_id}")))
            .body(Body::empty())
            .unwrap();
        let response = common::send(&app, read).await;
        assert_eq!(response.status(), StatusCode::OK);
        let view = common::read_json(response).await;
        assert_eq!(view["canAskSideQuestion"], !compacting, "{view}");
        for response in [
            call(
                &s,
                &configuration,
                "list_chat_side_questions",
                json!({ "chatId": chat_id }),
            )
            .await,
            public_call(
                &s,
                &["read"],
                "list_chat_side_questions",
                json!({ "chatId": chat_id }),
            )
            .await,
        ] {
            assert_eq!(
                result(&response)["canAskSideQuestion"],
                !compacting,
                "{response}"
            );
            if compacting {
                assert_eq!(result(&response)["sideQuestionUnavailable"], "compacting");
            }
        }
        for response in [
            call(&s, &configuration, "list_chats", json!({})).await,
            public_call(&s, &["read"], "list_chats", json!({})).await,
        ] {
            let view = result(&response)["chats"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == chat_id)
                .unwrap();
            assert_eq!(view["canAskSideQuestion"], !compacting, "{view}");
        }
        let body = || json!({ "id": uuid::Uuid::new_v4(), "text": "Why this order?" });
        let write = owner
            .authorize(common::request(
                "POST",
                &format!("/api/chats/{chat_id}/side-questions"),
            ))
            .body(Body::from(body().to_string()))
            .unwrap();
        let response = common::send(&app, write).await;
        assert_eq!(
            response.status(),
            if compacting {
                StatusCode::CONFLICT
            } else {
                StatusCode::OK
            }
        );
        for response in [
            call(
                &s,
                &configuration,
                "ask_chat_side_question",
                json!({ "chatId": chat_id, "question": body() }),
            )
            .await,
            public_call(
                &s,
                &["run"],
                "ask_chat_side_question",
                json!({ "chatId": chat_id, "question": body() }),
            )
            .await,
        ] {
            assert_eq!(
                response["result"]["isError"] == true,
                compacting,
                "{response}"
            );
            if compacting {
                assert!(response.to_string().contains("compacting"), "{response}");
            }
        }
    }
}

#[tokio::test]
async fn side_questions_are_asked_withdrawn_and_answered_beside_the_running_reply() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (codex, configuration) = active_run(&s, &main).await;
    let claude = s
        .agent_fixture(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    let (target, _) = active_run(&s, &claude).await;
    let run_id = target["id"].as_str().unwrap().to_owned();
    let chat_id = text(&target, "taskId").to_owned();
    s.store.event(
        &run_id,
        &cairn_contracts::EventKind::ItemCompleted,
        "First the migration",
        Some(json!({ "type": "item.completed", "item": { "id": "reply-1", "type": "agent_message", "text": "First the migration" } })),
    ).await.unwrap();
    let ask = |text: &str| {
        json!({
            "chatId": chat_id,
            "question": { "id": uuid::Uuid::new_v4(), "text": text, "quote": "First the migration", "itemId": "reply-1" },
        })
    };

    // Until the running reply's adapter announces it, side questions wait for it.
    let early = call(&s, &configuration, "ask_chat_side_question", ask("Why?")).await;
    assert_eq!(early["result"]["isError"], true, "{early}");
    assert!(
        early.to_string().contains("not offered side questions"),
        "{early}"
    );
    let listed = call(
        &s,
        &configuration,
        "list_chat_side_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&listed)["canAskSideQuestion"], false);
    let summary = call(&s, &configuration, "get_chat", json!({ "chatId": chat_id })).await;
    let reason = &result(&summary)["chat"]["sideQuestionReason"];
    assert!(
        reason
            .as_str()
            .unwrap()
            .contains("not offered side questions"),
        "{summary}"
    );
    assert_eq!(
        result(&summary)["chat"]["sideQuestionUnavailable"],
        "not-offered"
    );

    // A Claude Code release not qualified for side questions says so.
    s.side_questions_available(&run_id, false).await.unwrap();
    let old_release = call(&s, &configuration, "ask_chat_side_question", ask("Why?")).await;
    assert!(
        old_release
            .to_string()
            .contains("they need a stable release, 2.1.308 or later"),
        "{old_release}"
    );
    let listed = call(
        &s,
        &configuration,
        "list_chat_side_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&listed)["sideQuestionUnavailable"], "claude-release");

    s.side_questions_available(&run_id, true).await.unwrap();
    let unknown = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        json!({
            "chatId": chat_id,
            "question": { "id": uuid::Uuid::new_v4(), "text": "Why?", "itemId": "missing-reply" },
        }),
    )
    .await;
    assert_eq!(unknown["result"]["isError"], true, "{unknown}");
    assert!(unknown.to_string().contains("Reply not found"), "{unknown}");
    // Conversation lists report the same availability as the conversation itself.
    let chats = call(&s, &configuration, "list_chats", json!({})).await;
    let listed = result(&chats)["chats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|listed| listed["id"] == chat_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(listed["canAskSideQuestion"], true, "{listed}");
    let first = ask("Why this order?");
    let asked = call(&s, &configuration, "ask_chat_side_question", first.clone()).await;
    assert_eq!(result(&asked)["status"], "pending");
    assert_eq!(result(&asked)["runId"], run_id.as_str());
    assert_eq!(result(&asked)["provider"], "claude");
    assert!(!result(&asked)["replyEventId"].as_str().unwrap().is_empty());
    let repeated = call(&s, &configuration, "ask_chat_side_question", first.clone()).await;
    assert_eq!(result(&asked), result(&repeated));
    let mut reused = first.clone();
    reused["question"]["text"] = "Another question".into();
    let reused = call(&s, &configuration, "ask_chat_side_question", reused).await;
    assert_eq!(reused["result"]["isError"], true);
    assert!(reused.to_string().contains("already been used"), "{reused}");

    // The public MCP needs the run scope to ask, and reads with the read scope.
    let denied = public_call(&s, &["read"], "ask_chat_side_question", ask("Denied?")).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let second = public_call(
        &s,
        &["run"],
        "ask_chat_side_question",
        ask("And the tests?"),
    )
    .await;
    let second_id = result(&second)["id"].as_str().unwrap().to_owned();
    assert!(!result(&second)["replyEventId"].as_str().unwrap().is_empty());
    let public = public_call(
        &s,
        &["read"],
        "list_chat_side_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(
        result(&public)["sideQuestions"].as_array().unwrap().len(),
        2
    );

    // The running reply reads both from its inbox, apart from steering messages.
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inbox_path = s
        .config
        .data_dir
        .join("runs")
        .join(&run_id)
        .join("chat-input/messages.json");
    let inbox: Value = serde_json::from_slice(&std::fs::read(&inbox_path).unwrap()).unwrap();
    let delivered = inbox.as_array().unwrap();
    assert_eq!(delivered.len(), 2, "{inbox}");
    assert!(
        delivered
            .iter()
            .all(|entry| entry["sideQuestion"].is_object() && entry["text"].is_null())
    );
    assert_eq!(
        delivered[0]["sideQuestion"]["text"],
        "About this passage of your reply:\n> First the migration\n\nWhy this order?"
    );

    let withdrawn = call(
        &s,
        &configuration,
        "cancel_chat_side_question",
        json!({ "chatId": chat_id, "sideQuestionId": second_id }),
    )
    .await;
    assert_eq!(result(&withdrawn)["status"], "cancelled");
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inbox = std::fs::read_to_string(&inbox_path).unwrap();
    assert!(inbox.contains(&format!("withdraw-side-question:{second_id}")));

    let first_id = first["question"]["id"].as_str().unwrap();
    s.side_question_settle(
        &run_id,
        &json!({ "type": "chat.side.answered", "sideQuestionId": first_id, "text": "The migration must exist first." }),
    )
    .await
    .unwrap();
    // A late answer to a withdrawn question changes nothing.
    s.side_question_settle(
        &run_id,
        &json!({ "type": "chat.side.answered", "sideQuestionId": second_id, "text": "Too late." }),
    )
    .await
    .unwrap();
    let listed = call(
        &s,
        &configuration,
        "list_chat_side_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    let questions = result(&listed)["sideQuestions"].as_array().unwrap();
    assert_eq!(questions[0]["status"], "answered");
    assert_eq!(questions[0]["answer"], "The migration must exist first.");
    assert_eq!(questions[1]["status"], "cancelled");
    assert!(questions[1]["answer"].is_null());
    assert_eq!(result(&listed)["canAskSideQuestion"], true);

    // A run handed back to its queue has no adapter answering until it announces again.
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Queued),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let requeued = call(&s, &configuration, "ask_chat_side_question", ask("Queued?")).await;
    assert_eq!(requeued["result"]["isError"], true, "{requeued}");
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // A reply that is stopping, or switching coding agent, takes no new one.
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let stopping = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        ask("Stopping?"),
    )
    .await;
    assert!(stopping.to_string().contains("is stopping"), "{stopping}");
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Side questions never join the queue or the conversation.
    let detail = call(&s, &configuration, "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&detail)["messages"], json!([]));
    assert_eq!(result(&detail)["chat"]["pendingSideQuestions"], 0);

    // The next turn of the same run never answers a question about the previous one.
    let earlier = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        ask("Earlier turn?"),
    )
    .await;
    let earlier_id = result(&earlier)["id"].clone();
    s.store
        .update_run_boundary(
            &run_id,
            json!({ "chatExecution": { "messageId": "next-turn" } }),
        )
        .await
        .unwrap();
    // Nor does an answer that arrives once the turn moved on.
    s.side_question_settle(
        &run_id,
        &json!({ "type": "chat.side.answered", "sideQuestionId": earlier_id, "text": "Late." }),
    )
    .await
    .unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inbox = std::fs::read_to_string(&inbox_path).unwrap();
    assert!(!inbox.contains(earlier_id.as_str().unwrap()), "{inbox}");
    let listed = call(
        &s,
        &configuration,
        "list_chat_side_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    let earlier = result(&listed)["sideQuestions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|question| question["id"] == earlier_id)
        .unwrap()
        .clone();
    assert_eq!(earlier["status"], "failed", "{earlier}");

    // Asking and withdrawing again cannot grow a reply's side questions without bound.
    let mut refused = Value::Null;
    for index in 0..12 {
        let asked = call(
            &s,
            &configuration,
            "ask_chat_side_question",
            ask(&format!("Again {index}?")),
        )
        .await;
        if asked["result"]["isError"] == true {
            refused = asked;
            break;
        }
        let id = result(&asked)["id"].clone();
        call(
            &s,
            &configuration,
            "cancel_chat_side_question",
            json!({ "chatId": chat_id, "sideQuestionId": id }),
        )
        .await;
    }
    assert!(
        refused.to_string().contains("as many open side questions"),
        "{refused}"
    );
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inbox: Value = serde_json::from_slice(&std::fs::read(&inbox_path).unwrap()).unwrap();
    assert_eq!(inbox.as_array().unwrap().len(), 10, "{inbox}");
    // The next turn starts with none of them.
    s.store
        .update_run_boundary(
            &run_id,
            json!({ "chatExecution": { "messageId": "third-turn" } }),
        )
        .await
        .unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inbox: Value = serde_json::from_slice(&std::fs::read(&inbox_path).unwrap()).unwrap();
    assert_eq!(inbox, json!([]), "{inbox}");

    // A question about a whole reply carries the start of that reply to the agent.
    s.store
        .event(
            &run_id,
            &cairn_contracts::EventKind::ItemCompleted,
            "I traced the boundaries first.",
            Some(json!({
                "type": "item.completed",
                "item": { "id": "reply-7", "type": "agent_message", "text": "I traced the boundaries first." },
            })),
        )
        .await
        .unwrap();
    let whole = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        json!({
            "chatId": chat_id,
            "question": { "id": uuid::Uuid::new_v4(), "text": "Which boundaries?", "itemId": "reply-7" },
        }),
    )
    .await;
    assert_eq!(result(&whole)["status"], "pending", "{whole}");
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let inbox: Value = serde_json::from_slice(&std::fs::read(&inbox_path).unwrap()).unwrap();
    assert_eq!(
        inbox[0]["sideQuestion"]["text"],
        "About this reply of yours:\n> I traced the boundaries first.\n\nWhich boundaries?",
        "{inbox}"
    );

    // Once the reply ends, a question it never answered says so.
    let third = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        ask("Last one?"),
    )
    .await;
    let third_id = result(&third)["id"].clone();
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let listed = call(
        &s,
        &configuration,
        "list_chat_side_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    let expired = result(&listed)["sideQuestions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|question| question["id"] == third_id)
        .unwrap()
        .clone();
    assert_eq!(expired["status"], "failed");
    // Withdrawing it then changes nothing: it was not answered.
    let withdrawn = call(
        &s,
        &configuration,
        "cancel_chat_side_question",
        json!({ "chatId": chat_id, "sideQuestionId": third_id }),
    )
    .await;
    assert_eq!(result(&withdrawn)["status"], "failed", "{withdrawn}");
    assert!(
        expired["error"]
            .as_str()
            .unwrap()
            .contains("stopped before")
    );
    assert_eq!(result(&listed)["canAskSideQuestion"], false);
    let late = call(&s, &configuration, "ask_chat_side_question", ask("Now?")).await;
    assert_eq!(late["result"]["isError"], true);

    // A Codex release before `thread/sample` declines them, and the reason names Codex.
    let codex_id = codex["id"].as_str().unwrap();
    s.side_questions_available(codex_id, false).await.unwrap();
    let codex_chat_id = text(&codex, "taskId");
    let refused = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        json!({
            "chatId": codex_chat_id,
            "question": { "id": uuid::Uuid::new_v4(), "text": "Why?" },
        }),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        refused
            .to_string()
            .contains("This Codex release cannot answer"),
        "{refused}"
    );
    let listed = call(
        &s,
        &configuration,
        "list_chat_side_questions",
        json!({ "chatId": codex_chat_id }),
    )
    .await;
    assert_eq!(result(&listed)["canAskSideQuestion"], false);
    assert!(
        result(&listed)["sideQuestionReason"]
            .as_str()
            .unwrap()
            .contains("Codex")
    );
    // A Codex release that announced them takes one.
    s.side_questions_available(codex_id, true).await.unwrap();
    let asked = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        json!({
            "chatId": codex_chat_id,
            "question": { "id": uuid::Uuid::new_v4(), "text": "Why?" },
        }),
    )
    .await;
    assert_eq!(result(&asked)["status"], "pending", "{asked}");

    // A conversation moved to trash fails the side questions its stopped turn left.
    let (trashed_run, _) = active_run(&s, &claude).await;
    let trashed_run_id = trashed_run["id"].as_str().unwrap().to_owned();
    let trashed_id = text(&trashed_run, "taskId");
    s.side_questions_available(&trashed_run_id, true)
        .await
        .unwrap();
    let left = call(
        &s,
        &configuration,
        "ask_chat_side_question",
        json!({
            "chatId": trashed_id,
            "question": { "id": uuid::Uuid::new_v4(), "text": "Still there?" },
        }),
    )
    .await;
    assert_eq!(result(&left)["status"], "pending", "{left}");
    s.store
        .change_run(
            &trashed_run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Cancelled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.chat_trash(trashed_id, true).await.unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let stored = s
        .store
        .keys(&format!("chat-side-question:{trashed_id}:"))
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].1["status"], "failed", "{stored:?}");
}

/// Settings, overview and audit read the same on HTTP and both tools.
/// Removed S3 settings cannot be read, written or probed on any surface.
#[tokio::test]
async fn installation_settings_and_audit_match_and_s3_settings_are_removed_on_every_surface() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    std::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({
            "bucket": "cairn-disks", "endpoint": "https://s3.example.test", "region": "eu-west-3",
            "accessKeyId": "fixture-access-key", "secretAccessKey": "fixture-secret-key",
        })
        .to_string(),
    )
    .unwrap();
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);
    let member = common::RelayContext::new(
        &common::relay_fixture::member_context(&s, "fixture-member").await,
    );
    let public = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let http = async |who: &common::RelayContext, method: &str, path: &str, body: Value| {
        let request = who
            .authorize(common::request(method, path))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&app, request).await;
        (response.status(), common::read_json(response).await)
    };
    let public_call = async |name: &str, args: Value| {
        let request = common::request("POST", "/mcp")
            .header("authorization", format!("Bearer {public}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": common::mcp_arguments(name, &args),
                })
                .to_string(),
            ))
            .unwrap();
        common::read_json(common::send(&app, request).await).await
    };
    let (status, _) = http(
        &owner,
        "PUT",
        "/api/settings/conversations",
        json!({ "inactivityDays": 9 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for (name, path) in [
        ("get_installation_settings", "/api/settings"),
        ("get_node_runtime_settings", "/api/nodes/settings"),
        ("get_installation_overview", "/api/overview"),
        ("list_audit", "/api/audit"),
    ] {
        let (status, expected) = http(&owner, "GET", path, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{path}: {expected}");
        let tool = call(&s, &configuration, name, json!({})).await;
        assert_eq!(result(&tool), &expected, "{name}");
        let tool = public_call(name, json!({})).await;
        assert_eq!(result(&tool), &expected, "{name}");
        for response in [expected.to_string(), tool.to_string()] {
            assert!(
                !response.contains("fixture-access-key")
                    && !response.contains("fixture-secret-key")
            );
        }
        let (status, _) = http(&member, "GET", path, Value::Null).await;
        let member_allowed = path == "/api/overview";
        assert_eq!(status.is_success(), member_allowed, "{path}");
    }
    let (_, audit) = http(&owner, "GET", "/api/audit", Value::Null).await;
    let latest = &audit["entries"][0];
    assert_eq!(latest["action"], "conversation.retention.updated");
    assert_eq!(latest["detail"], json!({ "inactivityDays": 9 }).to_string());
    for internal in [false, true] {
        let settings = json!({
            "disconnectTimeoutSeconds": if internal { 40 } else { 30 },
            "shutdownTimeoutSeconds": 90,
        });
        let updated = if internal {
            call(
                &s,
                &configuration,
                "save_node_runtime_settings",
                settings.clone(),
            )
            .await
        } else {
            public_call("save_node_runtime_settings", settings.clone()).await
        };
        assert_eq!(result(&updated), &settings);
        assert_eq!(
            http(&owner, "GET", "/api/nodes/settings", Value::Null)
                .await
                .1,
            settings
        );
        for field in [
            "intervalSeconds",
            "budgetMiB",
            "maxCapacityWaitSeconds",
            "s3Configured",
        ] {
            let mut invalid = settings.clone();
            invalid[field] = 60.into();
            let refused = if internal {
                call(&s, &configuration, "save_node_runtime_settings", invalid).await
            } else {
                public_call("save_node_runtime_settings", invalid).await
            };
            assert_eq!(refused["result"]["isError"], true, "{field}: {refused}");
            assert_eq!(
                http(&owner, "GET", "/api/nodes/settings", Value::Null)
                    .await
                    .1,
                settings
            );
        }
    }

    let readonly = common::relay_fixture::grant(&s, &["read"]).await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &readonly,
        "save_node_runtime_settings",
        json!({ "disconnectTimeoutSeconds": 60, "shutdownTimeoutSeconds": 300 }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert!(
        denied["result"]["_meta"]["mcp/www_authenticate"]
            .as_str()
            .unwrap()
            .contains("manage")
    );
    assert_eq!(
        http(&member, "PUT", "/api/nodes/settings", json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let saved = std::fs::read(s.config.data_dir.join("storage-s3.json")).unwrap();
    for (name, method, path) in [
        ("get_storage_settings", "GET", "/api/settings/storage"),
        ("save_storage_settings", "PUT", "/api/settings/storage"),
        ("check_storage", "POST", "/api/settings/storage/check"),
    ] {
        let arguments = json!({
            "bucket": "other-disks",
            "endpoint": "https://s3.other.test",
            "region": "eu-west-3",
            "accessKeyId": "fixture-replacement-key",
            "secretAccessKey": "fixture-replacement-secret",
        });
        let (status, body) = http(&owner, method, path, arguments.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        assert!(!body.to_string().contains("fixture-secret-key"));
        let (status, _) = http(&member, method, path, arguments.clone()).await;
        assert!(status.is_client_error(), "{path}: {status}");

        for response in [
            call(&s, &configuration, name, arguments.clone()).await,
            public_call(name, arguments).await,
        ] {
            assert_eq!(response["result"]["isError"], true, "{response}");
            assert!(
                response.to_string().contains("Unknown operation"),
                "{response}"
            );
            assert!(!response.to_string().contains("fixture-secret-key"));
        }
    }
    assert_eq!(
        std::fs::read(s.config.data_dir.join("storage-s3.json")).unwrap(),
        saved,
        "Removed settings must preserve the operator's existing storage configuration"
    );
}
