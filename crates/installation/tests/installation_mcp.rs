use crate::common::run_fixture::DbRunFixture;
use cairn_contracts::EventKind;
#[allow(unused_imports)]
use common::{PluginServiceFixture as _, PluginStoreFixture as _};

use common::AgentFixture;
use common::McpFixture;
use common::MissionFixture;
use common::ProjectFixture;

mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::hex_digest, config::MAIN_AGENT_ID, mcps::channel, run_status::RunStatus,
    service::Service, validation::text,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

async fn service(root: &TempDir) -> Arc<Service> {
    std::fs::create_dir(root.path().join("home")).unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s
}

async fn active_run(s: &Service, agent: &Value) -> (Value, Value) {
    let task = s
        .task_fixture(
            json!({
                "name": "Inspect Cairn", "prompt": "Inspect runs", "agentId": agent["id"],
                "worktree": false,
            }),
            None,
        )
        .await
        .unwrap();
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    s.store
        .change_run(
            run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let run = s.store.run(text(&run, "id")).await.unwrap();
    let configuration = s.mcps.run_configuration(s, &run).await.unwrap();
    (run, configuration)
}

async fn enabled_main(s: &Arc<Service>) -> Value {
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap()
}

async fn request(
    s: &Arc<Service>,
    token: &str,
    method: &str,
    params: Value,
) -> (StatusCode, Value) {
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let request = common::request("POST", "/mcp-installation")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(&app, request).await;
    let status = response.status();
    (status, common::read_json(response).await)
}

fn create_profile(mut settings: Value) -> Value {
    let mut access = json!({
        "backend": "firecracker",
        "nodes": [],
        "projects": [],
        "skills": [],
        "mcps": [],
        "mcpTools": {},
        "installation": false,
        "delegation": false,
        "sandbox": "read-only",
    });
    if settings["provider"] == "opencode" {
        access["sandbox"] = "yolo".into();
    }
    if let Some(selected) = settings.get("access") {
        cairn_installation::store::merge(&mut access, selected);
    }
    settings["access"] = access;
    if settings.get("exclusiveNodes").is_none() {
        settings["exclusiveNodes"] = json!([]);
    }
    if settings.get("plugins").is_none() {
        settings["plugins"] = json!([]);
    }
    json!({ "settings": settings, "review": {} })
}

async fn replacement(s: &Service, args: Value) -> Value {
    let current = s
        .store
        .agent(args["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut settings = serde_json::to_value(&current.settings).unwrap();
    let mut patch = args["agent"].clone();
    let review = json!({
        "nativeNodeReviews": patch.as_object_mut().unwrap().remove("nativeNodeReviews").unwrap_or_else(|| json!([])),
        "reapproveDelegations": patch.as_object_mut().unwrap().remove("reapproveDelegations").unwrap_or_else(|| json!([])),
    });
    if let Some(access) = patch.get_mut("access") {
        let mut complete = settings["access"].clone();
        cairn_installation::store::merge(&mut complete, access);
        *access = complete;
    }
    cairn_installation::store::merge(&mut settings, &patch);
    json!({ "agentId": current.id, "revision": current.revision, "settings": settings, "review": review })
}

async fn call(s: &Arc<Service>, configuration: &Value, name: &str, args: Value) -> Value {
    let args = if name == "save_agent" && args.get("settings").is_none() {
        create_profile(args)
    } else if name == "replace_agent" && args.get("settings").is_none() {
        replacement(s, args).await
    } else if name == "delete_agent" && args.get("id").is_some() {
        json!({ "agentId": args["id"] })
    } else {
        args
    };
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (status, result) =
        request(s, token, "tools/call", common::mcp_arguments(name, &args)).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

fn result(response: &Value) -> &Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    &response["result"]["structuredContent"]["result"]
}

async fn node_management_call(
    app: &axum::Router,
    endpoint: &str,
    token: &str,
    name: &str,
    args: Value,
) -> (StatusCode, Value) {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": common::mcp_arguments(name, &args),
    });
    let request = common::request("POST", endpoint)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(app, request).await;
    (response.status(), common::read_json(response).await)
}

async fn enroll_management_node(app: &axum::Router, invitation: &Value) -> Value {
    let backend = invitation["backend"].as_str().unwrap();
    let body = json!({
        "code": invitation["code"],
        "name": "Desk",
        "protocol": 1,
        "runtimeId": "development",
        "capabilities": {
            "os": "linux",
            "arch": "x86_64",
            "backend": backend,
            "kvm": backend == "firecracker",
            "ublk": true,
            "cpu": 8,
            "memoryMiB": 16384,
            "diskMiB": 65536,
        },
    });
    let request = common::request("POST", "/internal/nodes/enroll")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let identity = common::read_json(response).await;
    assert_eq!(identity["nodeId"], invitation["nodeId"]);
    identity
}

#[tokio::test]
async fn pending_recovery_can_be_inspected_and_cancelled_through_both_tool_interfaces() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "run", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        for chat_retry in [true, false] {
            let chat = s.chat_create(json!({})).await.unwrap();
            s.chat_send(
                chat["id"].as_str().unwrap(),
                json!({ "text": "Keep this request", "id": uuid::Uuid::new_v4().to_string() }),
            )
            .await
            .unwrap();
            s.chat_tick(&std::collections::HashSet::default())
                .await
                .unwrap();
            let chat = s.chat_detail(chat["id"].as_str().unwrap()).await.unwrap();
            let run_id = chat["runId"].as_str().unwrap();
            let retry = json!({
                "attempt": 2,
                "limit": 5,
                "nextAttemptAt": cairn_installation::config::now() + 60_000,
                "startedAt": cairn_installation::config::now(),
                "deadlineAt": cairn_installation::config::now() + 600_000,
                "cause": { "kind": "timeout", "message": "Temporary timeout", "startup": true },
            });
            s.store
                .change_run(
                    run_id,
                    cairn_installation::runs::RunChanges {
                        retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let (_, history) =
                node_management_call(&app, endpoint, token, "get_run", json!({ "runId": run_id }))
                    .await;
            assert_eq!(result(&history)["run"]["run"]["retry"], retry);
            let (name, args) = if chat_retry {
                ("stop_chat", json!({ "chatId": chat["id"] }))
            } else {
                ("cancel_run", json!({ "runId": run_id }))
            };
            let (status, response) = node_management_call(&app, endpoint, token, name, args).await;
            assert_eq!(status, StatusCode::OK, "{response}");
            result(&response);
            let stopped = s.store.run(run_id).await.unwrap();
            assert_eq!(stopped["status"], RunStatus::Cancelled);
            assert!(stopped["retry"].is_null());
            let retained = s.chat_detail(chat["id"].as_str().unwrap()).await.unwrap();
            assert_eq!(retained["messages"].as_array().unwrap().len(), 1);
            if chat_retry {
                assert_eq!(retained["paused"], true);
            }
        }
    }
}

#[tokio::test]
async fn node_switch_controls_match_public_mcp_and_installation_tools_and_keep_their_grants() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let trusted = uuid::Uuid::new_v4().to_string();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        s.store
            .set(
                "native-authority",
                json!({
                    "observedAt": cairn_installation::config::now(),
                    "accounts": { (trusted.clone()): { "email": "owner@example.test" } },
                }),
                None,
            )
            .await
            .unwrap();
        let (_, callers) =
            node_management_call(&app, endpoint, token, "list_native_callers", json!({})).await;
        assert_eq!(result(&callers)[0]["id"], trusted);
        let mut identities = Vec::new();
        for backend in ["firecracker", "native"] {
            let (_, response) = node_management_call(
                &app,
                endpoint,
                token,
                "create_node_enrollment",
                json!({
                    "name": "Desk", "backend": backend,
                }),
            )
            .await;
            identities.push(enroll_management_node(&app, result(&response)).await);
        }
        let previous = identities[0]["nodeId"].as_str().unwrap();
        let next = identities[1]["nodeId"].as_str().unwrap();
        let (_, updated) = node_management_call(
            &app,
            endpoint,
            token,
            "update_node",
            json!({
                "nodeId": next,
                "node": {
                    "name": "Trusted desktop", "tags": ["gpu"],
                    "limits": { "cpu": 2, "memoryMiB": 2048, "diskMiB": 8192 },
                    "slots": 6, "accepting": true,
                    "nativeEnabled": true, "nativeAccounts": [trusted],
                },
            }),
        )
        .await;
        assert_eq!(result(&updated)["nativeEnabled"], true);
        assert_eq!(result(&updated)["capabilities"]["backend"], "native");
        let mut capable = s.get("nodes", next).await.unwrap();
        capable["capabilities"]["nativeModeProtocol"] = 1.into();
        s.store.put("nodes", capable).await.unwrap();
        let (_, preview) = node_management_call(
            &app,
            endpoint,
            token,
            "change_native_node_mode",
            json!({
                "nodeId": next, "change": { "mode": "administrator" },
            }),
        )
        .await;
        let preview = result(&preview);
        assert!(
            preview["preview"]["warning"]
                .as_str()
                .unwrap()
                .contains("real host")
        );
        let (_, requested) = node_management_call(&app, endpoint, token, "change_native_node_mode", json!({
            "nodeId": next, "change": { "mode": "administrator", "previewId": preview["previewId"], "confirm": true },
        })).await;
        if endpoint == "/mcp-installation" {
            assert_eq!(
                result(&requested)["status"],
                "awaiting-owner",
                "{requested}"
            );
            assert!(s.get("nodes", next).await.unwrap()["nativeModeRequest"].is_null());
        } else {
            assert_eq!(
                result(&requested)["nativeModeStatus"],
                "pending",
                "{requested}"
            );
            assert_eq!(
                result(&requested)["nativeModeRequest"]["mode"],
                "administrator"
            );
        }

        let (_, listed) =
            node_management_call(&app, endpoint, token, "list_nodes", json!({})).await;
        let node = result(&listed)
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == next)
            .unwrap();
        assert_eq!(node["nativeAccounts"], json!([trusted]));
        assert_eq!(node["slots"], 6);
        s.store
            .update_run_boundary(
                run["id"].as_str().unwrap(),
                json!({ "pinnedNodeId": previous }),
            )
            .await
            .unwrap();
        let (_, retired) = node_management_call(
            &app,
            endpoint,
            token,
            "revoke_node",
            json!({
                "nodeId": previous, "releasePlacements": true,
            }),
        )
        .await;
        assert_eq!(result(&retired)["revoked"], true);
        assert_eq!(result(&retired)["releasedPlacements"], 1);
        assert!(s.store.run(run["id"].as_str().unwrap()).await.unwrap()["pinnedNodeId"].is_null());
        // Ordinary revocation preserves an explicit preference, as the owner UI does.
        s.store
            .update_run_boundary(
                run["id"].as_str().unwrap(),
                json!({ "preferredNodeId": next }),
            )
            .await
            .unwrap();
        let (_, retired) = node_management_call(
            &app,
            endpoint,
            token,
            "revoke_node",
            json!({ "nodeId": next }),
        )
        .await;
        assert_eq!(result(&retired)["releasedPlacements"], 0);
        assert_eq!(
            s.store.run(run["id"].as_str().unwrap()).await.unwrap()["preferredNodeId"],
            next
        );
    }

    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read_token,
        "create_node_enrollment",
        json!({ "name": "Denied" }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert!(denied["result"]["_meta"]["mcp/www_authenticate"].is_string());
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let (status, _) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "create_node_enrollment",
        json!({ "name": "Denied" }),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
}

#[tokio::test]
async fn node_storage_controls_share_probes_persistence_and_permissions_across_both_tools() {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let probes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let fail_probe = Arc::new(AtomicBool::new(false));
    let recorded = probes.clone();
    let failure = fail_probe.clone();
    let controller = axum::Router::new().route(
        "/internal/execution/{node}/storage-policy",
        axum::routing::post(move |axum::Json(policy): axum::Json<Value>| {
            let recorded = recorded.clone();
            let failure = failure.clone();
            async move {
                recorded.lock().unwrap().push(policy);
                if failure.load(Ordering::SeqCst) {
                    return StatusCode::CONFLICT;
                }
                StatusCode::OK
            }
        }),
    );
    let (url, server) = common::serve_locally(controller).await;
    let mut config = common::config(root.path());
    config.public_url = url;
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    // Synthetic successful AWS CLI responses; no real object store or credentials.
    std::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "cairn-storage-test", "awsBinary": "/bin/true" }).to_string(),
    )
    .unwrap();
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let policy = json!({
        "cacheMiB": 4096, "memoryCacheMiB": 512,
        "reserveMiB": 2048, "reservePercent": 7,
        "backupSeconds": 30, "maxDirtySeconds": 120,
    });
    let mut last_node = String::new();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, invitation) = node_management_call(
            &app,
            endpoint,
            token,
            "create_node_enrollment",
            json!({ "name": "Storage host", "backend": "native" }),
        )
        .await;
        let identity = enroll_management_node(&app, result(&invitation)).await;
        let node = identity["nodeId"].as_str().unwrap();
        let (_, updated) = node_management_call(
            &app,
            endpoint,
            token,
            "update_node_storage",
            json!({ "nodeId": node, "storage": policy }),
        )
        .await;
        assert_eq!(result(&updated)["storage"], policy);
        assert_eq!(probes.lock().unwrap().last(), Some(&policy));
        let (_, listed) =
            node_management_call(&app, endpoint, token, "list_nodes", json!({})).await;
        let saved = result(&listed)
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == node)
            .unwrap();
        assert_eq!(saved["storage"], policy);

        fail_probe.store(true, Ordering::SeqCst);
        let mut rejected = policy.clone();
        rejected["memoryCacheMiB"] = 1024.into();
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "update_node_storage",
            json!({ "nodeId": node, "storage": rejected }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(s.get("nodes", node).await.unwrap()["storage"], policy);
        fail_probe.store(false, Ordering::SeqCst);
        last_node = node.to_owned();
    }
    let before = probes.lock().unwrap().len();
    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read_token,
        "update_node_storage",
        json!({ "nodeId": last_node, "storage": policy }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let (status, _) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "update_node_storage",
        json!({ "nodeId": last_node, "storage": policy }),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
    assert_eq!(probes.lock().unwrap().len(), before);
    server.abort();
}

#[tokio::test]
async fn agent_catalog_accepts_current_providers_and_reasoning_through_native_mcp() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (_, catalog) = request(&s, token, "tools/list", json!({})).await;
    let tools = catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["search", "execute"]
    );
    let catalog = call(
        &s,
        &configuration,
        "search",
        json!({ "names": ["save_agent", "replace_agent"], "detail": "schema" }),
    )
    .await;
    let operations = result(&catalog)["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 2);
    for name in ["save_agent", "replace_agent"] {
        let tool = operations.iter().find(|tool| tool["name"] == name).unwrap();
        let schema = &tool["inputSchema"]["definitions"]["AgentSettings"];
        assert_eq!(
            schema["properties"]["provider"]["pattern"],
            json!("^(codex|claude|opencode)$")
        );
        assert!(schema["properties"]["reasoning"].get("enum").is_none());
    }

    let saved = call(
        &s,
        &configuration,
        "save_agent",
        json!({ "name": "Default engine" }),
    )
    .await;
    assert_eq!(result(&saved)["provider"], "codex");
    assert_eq!(result(&saved)["reasoning"], "high");

    for provider in ["codex", "claude", "opencode"] {
        let saved = call(
            &s,
            &configuration,
            "save_agent",
            json!({
                "name": "Selected engine",
                "provider": provider,
                "model": if provider == "opencode" { "opencode/test-model" } else { "" },
                "reasoning": if provider == "claude" { "max" } else { "future_effort-2" },
            }),
        )
        .await;
        let agent = result(&saved);
        assert_eq!(agent["provider"], provider);
        assert_eq!(
            agent["reasoning"],
            if provider == "claude" {
                "max"
            } else {
                "future_effort-2"
            }
        );
        assert_eq!(
            s.agent_snapshot(agent["id"].as_str().unwrap())
                .await
                .unwrap()["provider"],
            provider
        );
    }

    for invalid in [
        json!({ "provider": "unknown" }),
        json!({ "provider": null }),
        json!({ "reasoning": "INVALID" }),
        json!({ "reasoning": "a".repeat(41) }),
    ] {
        let mut args = json!({ "name": "Invalid profile" });
        args.as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        let rejected = call(&s, &configuration, "save_agent", args).await;
        assert_eq!(rejected["result"]["isError"], true, "{rejected}");
    }
}

#[tokio::test]
async fn agent_replacements_keep_explicit_settings_and_accept_new_engine_settings() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let saved = s
        .agent_fixture(
            json!({
                "name": "Restricted Claude",
                "provider": "claude",
                "reasoning": "max",
                "model": "claude-opus-4-6",
                "instructions": "Preserve these settings",
                "timeoutMinutes": 37,
                "access": { "nodes": [], "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let original = &saved;
    let updated = call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": original["id"],
            "agent": { "description": "New description" },
        }),
    )
    .await;
    let agent = result(&updated);
    for field in [
        "provider",
        "reasoning",
        "model",
        "instructions",
        "timeoutMinutes",
        "access",
    ] {
        assert_eq!(agent[field], original[field], "{field}");
    }

    let updated = call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": original["id"],
            "agent": {
                "provider": "opencode",
                "model": "opencode/test-model",
                "reasoning": "future_effort-2",
                "access": {},
            },
        }),
    )
    .await;
    let agent = result(&updated);
    assert_eq!(agent["provider"], "opencode");
    assert_eq!(agent["reasoning"], "future_effort-2");
    assert_eq!(agent["access"]["nodes"], json!([]));
    assert_eq!(agent["access"]["installation"], true);

    let updated = call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": original["id"],
            "agent": { "reasoning": "" },
        }),
    )
    .await;
    assert_eq!(result(&updated)["reasoning"], "");
    assert_eq!(result(&updated)["provider"], "opencode");

    for patch in [
        json!({ "provider": "unknown" }),
        json!({ "provider": null }),
        json!({ "reasoning": "INVALID" }),
    ] {
        let before = s
            .agent_snapshot(original["id"].as_str().unwrap())
            .await
            .unwrap();
        let rejected = call(
            &s,
            &configuration,
            "replace_agent",
            json!({
                "id": original["id"],
                "agent": patch,
            }),
        )
        .await;
        assert_eq!(rejected["result"]["isError"], true, "{rejected}");
        assert_eq!(
            s.agent_snapshot(original["id"].as_str().unwrap())
                .await
                .unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn model_speeds_and_agent_speeds_match_public_mcp_and_installation_tools() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let config = cairn_installation::config::Config {
        codex_bin: common::fixture("codex.mjs"),
        ..common::config(root.path())
    };
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;

    let internal = call(&s, &configuration, "list_models", json!({})).await;
    let public = public_call(&s, &["read"], "list_models", json!({ "provider": "codex" })).await;
    assert_eq!(result(&internal), result(&public));
    let fast = result(&public)["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["model"] == "fixture-fast")
        .unwrap()
        .clone();
    assert_eq!(fast["serviceTiers"][0]["id"], "priority");
    assert_eq!(fast["serviceTiers"][0]["name"], "Fast");
    let denied = public_call(&s, &["run"], "list_models", json!({})).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");

    let saved = call(
        &s,
        &configuration,
        "save_agent",
        json!({ "name": "Fast reviewer", "speed": "priority" }),
    )
    .await;
    let agent = result(&saved).clone();
    assert_eq!(agent["speed"], "priority");
    let kept = public_call(
        &s,
        &["manage"],
        "replace_agent",
        json!({ "id": agent["id"], "agent": { "description": "Keeps its speed" } }),
    )
    .await;
    assert_eq!(result(&kept)["speed"], "priority");
    let standard = call(
        &s,
        &configuration,
        "replace_agent",
        json!({ "id": agent["id"], "agent": { "speed": "standard" } }),
    )
    .await;
    assert_eq!(result(&standard)["speed"], "standard");

    // Speed belongs to Codex: other coding agents reject it, and switching drops it.
    let rejected = public_call(
        &s,
        &["manage"],
        "save_agent",
        json!({ "name": "Claude", "provider": "claude", "speed": "priority" }),
    )
    .await;
    assert_eq!(rejected["result"]["isError"], true, "{rejected}");
    let rejected = call(
        &s,
        &configuration,
        "replace_agent",
        json!({ "id": agent["id"], "agent": { "provider": "claude", "speed": "priority" } }),
    )
    .await;
    assert_eq!(rejected["result"]["isError"], true, "{rejected}");
    let switched = call(
        &s,
        &configuration,
        "replace_agent",
        json!({ "id": agent["id"], "agent": { "provider": "claude", "reasoning": "max", "speed": "" } }),
    )
    .await;
    assert_eq!(result(&switched)["provider"], "claude");
    assert_eq!(result(&switched)["speed"], "");

    // A queued message keeps its speed through edits that do not send one, on both tools.
    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    s.chat_pause(chat_id, true).await.unwrap();
    let message_id = uuid::Uuid::new_v4().to_string();
    let sent = public_call(
        &s,
        &["run"],
        "send_chat_message",
        json!({
            "chatId": chat_id,
            "message": { "id": message_id, "text": "Next", "provider": "codex", "speed": "priority" },
        }),
    )
    .await;
    assert_eq!(result(&sent)["speed"], "priority", "{sent}");
    let edited = public_call(
        &s,
        &["run"],
        "edit_chat_message",
        json!({ "chatId": chat_id, "messageId": message_id, "message": { "text": "Reworded" } }),
    )
    .await;
    assert_eq!(result(&edited)["speed"], "priority", "{edited}");
    let edited = call(
        &s,
        &configuration,
        "edit_chat_message",
        json!({ "chatId": chat_id, "messageId": message_id, "message": { "text": "Again" } }),
    )
    .await;
    assert_eq!(result(&edited)["speed"], "priority", "{edited}");
    let standard = call(
        &s,
        &configuration,
        "edit_chat_message",
        json!({ "chatId": chat_id, "messageId": message_id, "message": { "text": "Again", "speed": "standard" } }),
    )
    .await;
    assert_eq!(result(&standard)["speed"], "standard", "{standard}");
}

#[tokio::test]
async fn installation_access_is_opt_in_and_complete_profile_replacements_preserve_it() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    let (_, disabled) = active_run(&s, &main).await;
    assert!(disabled["codexConfig"].get("cairn_installation").is_none());
    assert!(
        disabled["claudeMcps"]["mcpServers"]
            .get("cairn_installation")
            .is_none()
    );
    assert_eq!(
        request(&s, "invalid", "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );

    let enabled = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &enabled).await;
    assert_eq!(
        configuration["codexConfig"]["cairn_installation"]["url"],
        "http://localhost:4310/mcp-installation"
    );
    assert!(configuration.get("claudeMcps").is_none());
    let claude = s
        .agent_fixture(
            json!({
                "name": "Claude", "provider": "claude",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let (_, claude_configuration) = active_run(&s, &claude).await;
    assert!(claude_configuration.get("codexConfig").is_none());
    assert_eq!(
        claude_configuration["claudeMcps"]["mcpServers"]["cairn_installation"]["url"],
        "http://localhost:4310/mcp-installation"
    );
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    assert!(
        configuration["redactions"]
            .as_array()
            .unwrap()
            .contains(&json!(token))
    );
    let saved = s
        .agent_fixture(
            json!({ "name": "Older editor", "access": { "nodes": null } }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
    assert_eq!(saved["access"]["installation"], true);
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn installation_tools_schedule_inspect_update_run_and_cancel_missions() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (_, catalog) = request(&s, token, "tools/list", json!({})).await;
    let native = catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(
        native
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["search", "execute"]
    );
    assert!(native.iter().all(|tool| tool.get("_meta").is_none()));
    assert!(serde_json::to_vec(native).unwrap().len() < 6000);
    let mut tools = Vec::new();
    let mut offset = 0;
    loop {
        let page = call(
            &s,
            &configuration,
            "search",
            json!({ "detail": "schema", "limit": 20, "offset": offset }),
        )
        .await;
        tools.extend(result(&page)["operations"].as_array().unwrap().clone());
        let Some(next) = result(&page)["nextOffset"].as_u64() else {
            break;
        };
        offset = next;
    }
    for name in [
        "create_task",
        "list_tasks",
        "get_run",
        "run_task",
        "delete_agent",
    ] {
        assert!(tools.iter().any(|tool| tool["name"] == name));
    }
    let public: Value =
        serde_json::from_str(include_str!("../schemas/mcp-operations.json")).unwrap();
    let mut public_names: std::collections::BTreeSet<_> = public
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let installed_names: std::collections::BTreeSet<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    public_names.extend([
        "list_mcps",
        "create_mcp",
        "update_mcp",
        "test_mcp",
        "disconnect_mcp",
        "delete_mcp",
        "begin_mcp_connection",
        "mcp_connection_status",
        "cancel_mcp_connection",
    ]);
    public_names.extend([
        "run_task",
        "retry_run",
        "cancel_run",
        "cleanup_run",
        "list_runs",
        "get_run",
        "read_run_history",
        "read_run_content",
    ]);
    public_names.insert(cairn_contracts::RESUME_RUN_ID);
    public_names.insert("manage_plugin");
    public_names.extend([
        "list_plugins",
        "install_plugin_package",
        "approve_plugin_package",
        "activate_plugin_package",
        "delete_plugin_package",
        "list_plugin_programs",
        "prepare_plugin_program",
        "get_plugin_operation",
        "cancel_plugin_operation",
    ]);
    public_names.extend([
        "set_agent_avatar",
        "list_agents",
        "get_agent",
        "save_agent",
        "replace_agent",
        "preview_agent",
        "merge_agents",
        "delete_agent",
        "get_agent_avatar_configuration",
        "generate_agent_avatar",
        "list_tasks",
        "get_task",
        "create_task",
        "replace_task",
        "set_task_state",
        "delete_task",
        "preview_schedule",
        "list_task_activity",
        "list_projects",
        "get_project",
        "save_project",
        "replace_project",
        "delete_project",
    ]);
    assert_eq!(installed_names, public_names);
    assert!(tools.iter().all(|tool| tool.get("_meta").is_none()));
    let deletion = tools
        .iter()
        .find(|tool| tool["name"] == "delete_agent")
        .unwrap();
    assert_eq!(deletion["annotations"]["destructiveHint"], true);
    assert_eq!(deletion["annotations"]["readOnlyHint"], false);

    let created = call(
        &s,
        &configuration,
        "create_task",
        json!({ "settings": {
            "name": "Weekly audit", "prompt": "Audit", "agentId": MAIN_AGENT_ID,
            "cron": "0 9 * * 1", "timezone": "UTC", "worktree": false,
        } }),
    )
    .await;
    let task = result(&created);
    assert!(task["nextRun"].is_number());
    assert_eq!(task["authorId"], "fixture-owner");
    let tasks = call(&s, &configuration, "list_tasks", json!({})).await;
    assert!(
        result(&tasks)["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == task["id"])
    );
    let mut update = task.clone();
    update["enabled"] = false.into();
    let updated = call(
        &s,
        &configuration,
        "replace_task",
        json!({ "taskId": task["id"], "revision": task["revision"], "settings": mission_settings(&update) }),
    )
    .await;
    assert_eq!(result(&updated)["nextRun"], Value::Null);
    let queued = call(
        &s,
        &configuration,
        "run_task",
        json!({ "taskId": task["id"] }),
    )
    .await;
    let queued_id = result(&queued)["id"].clone();
    // The mission remembers which agent work started it, so it can wait on that work.
    let requested_by = &s.store.run(queued_id.as_str().unwrap()).await.unwrap()["requestedBy"];
    assert_eq!(requested_by["runId"], run["id"]);
    assert_eq!(requested_by["taskId"], run["taskId"]);
    let cancelled = call(
        &s,
        &configuration,
        "cancel_run",
        json!({ "runId": queued_id }),
    )
    .await;
    assert_eq!(result(&cancelled)["cancelled"], true);
    assert_eq!(
        s.store.run(queued_id.as_str().unwrap()).await.unwrap()["status"],
        RunStatus::Cancelled
    );
    // The engine's own retries and usage limits read as the app shows them.
    let engine =
        json!({ "type": "engine.retry", "attempt": 2, "maxRetries": 10, "reason": "overloaded" });
    s.store
        .event(
            run["id"].as_str().unwrap(),
            &EventKind::EngineRetry,
            "retry",
            Some(engine.clone()),
        )
        .await
        .unwrap();
    let history = call(&s, &configuration, "get_run", json!({ "runId": run["id"] })).await;
    assert_eq!(result(&history)["run"]["run"]["id"], run["id"]);
    let events = result(&history)["events"].as_array().unwrap().clone();
    let window = call(
        &s,
        &configuration,
        "read_run_history",
        json!({
            "runId": run["id"], "history": result(&history)["history"],
        }),
    )
    .await;
    assert_ne!(window["result"]["isError"], true);
    assert!(
        result(&window)["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["event"]["type"] == "engine.retry")
    );

    assert!(
        events
            .iter()
            .any(|record| record["event"]["type"] == "engine.retry"
                && record["event"]["payload"] == engine)
    );
    let chunk = call(
        &s,
        &configuration,
        "read_run_content",
        json!({ "runId": run["id"] }),
    )
    .await;
    assert_eq!(result(&chunk)["runId"], run["id"]);
}

#[tokio::test]
async fn delete_agent_removes_an_unused_profile_and_preserves_completed_history() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent = s
        .agent_fixture(json!({ "name": "Retired maintainer" }), None)
        .await
        .unwrap();
    let id = agent["id"].as_str().unwrap();
    let (run, _) = active_run(&s, &agent).await;
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
    s.remove_mission_fixture(run["taskId"].as_str().unwrap())
        .await
        .unwrap();
    s.store
        .transaction({
            let id = id.to_owned();
            move |db| {
                db.set(
                    &format!("agent-avatar:{id}"),
                    &json!({ "status": "ready" }),
                    None,
                )?;
                // A plugin instance (such as a 1Password service account) granting it.
                let mut instance = cairn_installation::plugins::connections::Connection::draft(
                    "00000000-0000-4000-8000-000000000099",
                    "onepassword",
                    json!({}),
                    Vec::new(),
                );
                instance.name = "Fixture".into();
                instance.revision = "r1".into();
                {
                    db.save_plugin_version(&instance.plugin_id, "fixture", &"0".repeat(64))?;
                    db.save_plugin_instance(&instance)
                }?;
                for agent_id in [id.as_str(), MAIN_AGENT_ID] {
                    let mut agent = db.agent(agent_id)?.unwrap();
                    agent
                        .settings
                        .plugins
                        .push("00000000-0000-4000-8000-000000000099".into());
                    db.save_agent(&agent)?;
                }
                Ok(())
            }
        })
        .await
        .unwrap();

    let deleted = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(result(&deleted)["deleted"], true);
    assert!(s.store.agent_snapshot(id).await.unwrap().is_none());
    assert!(
        s.store
            .kv(&format!("agent-avatar:{id}"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        s.store
            .instance_fixture("00000000-0000-4000-8000-000000000099")
            .await
            .unwrap()
            .unwrap()["agentIds"],
        json!([MAIN_AGENT_ID])
    );
    assert_eq!(
        s.store.run(run["id"].as_str().unwrap()).await.unwrap()["snapshot"]["agent"]["id"],
        id
    );
    let missing = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(missing["result"]["isError"], true);
}

#[tokio::test]
async fn delete_agent_rejects_main_task_references_and_active_work() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let main_delete = call(
        &s,
        &configuration,
        "delete_agent",
        json!({ "id": MAIN_AGENT_ID }),
    )
    .await;
    assert_eq!(main_delete["result"]["isError"], true);
    assert_eq!(
        main_delete["result"]["content"][0]["text"],
        "The main agent cannot be removed."
    );

    let agent = s
        .agent_fixture(json!({ "name": "Assigned maintainer" }), None)
        .await
        .unwrap();
    let id = agent["id"].as_str().unwrap();
    let task = s
        .task_fixture(
            json!({
                "name": "Archived maintenance",
                "prompt": "Maintain",
                "agentId": id,
                "archived": true,
            }),
            None,
        )
        .await
        .unwrap();
    let assigned = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(assigned["result"]["isError"], true);
    assert_eq!(
        assigned["result"]["content"][0]["text"],
        "This item is used by a task. Update or remove that task first."
    );
    s.remove_mission_fixture(task["id"].as_str().unwrap())
        .await
        .unwrap();

    let (run, _) = active_run(&s, &agent).await;
    let task_id = run["taskId"].as_str().unwrap();
    let mut task = s.mission_snapshot_fixture(task_id).await.unwrap();
    task["agentId"] = MAIN_AGENT_ID.into();
    s.task_fixture(task, Some(task_id)).await.unwrap();
    let busy = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(busy["result"]["isError"], true);
    assert_eq!(
        busy["result"]["content"][0]["text"],
        "This item has active work. Cancel or wait for the run first."
    );
    assert!(s.store.agent_snapshot(id).await.unwrap().is_some());
}

#[tokio::test]
async fn delete_agent_cannot_use_a_revoked_installation_grant() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let agent = s
        .agent_fixture(json!({ "name": "Keep this profile" }), None)
        .await
        .unwrap();
    s.mcps
        .revoke_run(&s, run["id"].as_str().unwrap())
        .await
        .unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (status, _) = request(
        &s,
        token,
        "tools/call",
        json!({
            "name": "delete_agent",
            "arguments": { "agentId": agent["id"] },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        s.store
            .agent_snapshot(agent["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn merge_agents_cannot_use_a_revoked_installation_grant() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let agent = s
        .agent_fixture(json!({ "name": "Keep this profile" }), None)
        .await
        .unwrap();
    s.mcps
        .revoke_run(&s, run["id"].as_str().unwrap())
        .await
        .unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (status, _) = request(
        &s,
        token,
        "tools/call",
        common::mcp_arguments(
            "merge_agents",
            &json!({
                "sourceId": agent["id"], "targetId": MAIN_AGENT_ID, "confirm": true,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        s.store
            .agent_snapshot(agent["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn installation_access_manages_resources_and_other_agents_work() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    let (foreign, _) = active_run(&s, &main).await;
    let manager = s
        .agent_fixture(
            json!({
                "name": "Manager",
                "access": {
                    "installation": true,
                    "projects": [],
                    "skills": [],
                    "mcps": [],
                    "sandbox": "read-only"
                }
            }),
            None,
        )
        .await
        .unwrap();
    let (_, configuration) = active_run(&s, &manager).await;

    let saved = call(
        &s,
        &configuration,
        "save_agent",
        json!({ "name": "Created through MCP" }),
    )
    .await;
    let agent = result(&saved);
    let updated = call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": agent["id"],
            "agent": { "description": "Updated through MCP" }
        }),
    )
    .await;
    assert_eq!(result(&updated)["description"], "Updated through MCP");
    let agents = call(&s, &configuration, "list_agents", json!({})).await;
    assert!(
        result(&agents)["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == MAIN_AGENT_ID)
    );

    let project_path = root.path().join("managed-project");
    std::fs::create_dir(&project_path).unwrap();
    let project = call(
        &s,
        &configuration,
        "save_project",
        json!({
            "settings": { "name": "Managed project", "path": project_path, "sourceMode": "local" }
        }),
    )
    .await;
    let project_id = result(&project)["id"].clone();
    let projects = call(&s, &configuration, "list_projects", json!({})).await;
    assert!(
        result(&projects)["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == project_id)
    );

    let connection = call(
        &s,
        &configuration,
        "create_mcp",
        json!({
            "name": "Managed connection",
            "url": "https://example.com/mcp",
            "enabled": false
        }),
    )
    .await;
    let connection_id = result(&connection)["id"].clone();
    let updated = call(
        &s,
        &configuration,
        "update_mcp",
        json!({
            "id": connection_id,
            "revision": result(&connection)["revision"],
            "connection": {
                "name": "Renamed connection",
                "url": "https://example.com/mcp",
                "enabled": false,
                "enabledTools": null,
                "allowPrivateNetwork": false,
                "transport": "http",
                "command": "",
                "args": [],
                "auth": "none",
                "clientId": "",
                "scopes": ""
            }
        }),
    )
    .await;
    assert_eq!(result(&updated)["name"], "Renamed connection");
    let connections = call(&s, &configuration, "list_mcps", json!({})).await;
    assert!(
        result(&connections)["mcps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == connection_id)
    );
    let disconnected = call(
        &s,
        &configuration,
        "disconnect_mcp",
        json!({ "id": connection_id }),
    )
    .await;
    assert_eq!(result(&disconnected)["completed"], true);
    let deleted = call(
        &s,
        &configuration,
        "delete_mcp",
        json!({ "id": connection_id }),
    )
    .await;
    assert_eq!(result(&deleted)["completed"], true);
    let connections = call(&s, &configuration, "list_mcps", json!({})).await;
    assert!(result(&connections)["mcps"].as_array().unwrap().is_empty());

    let created = call(
        &s,
        &configuration,
        "create_task",
        json!({ "settings": {
            "name": "Delegate",
            "prompt": "Inspect",
            "agentId": MAIN_AGENT_ID,
            "worktree": false
        } }),
    )
    .await;
    assert_eq!(result(&created)["agentId"], MAIN_AGENT_ID);
    let own_default = call(
        &s,
        &configuration,
        "create_task",
        json!({ "settings": {
            "name": "Continue",
            "prompt": "Continue",
            "worktree": false
        } }),
    )
    .await;
    assert_eq!(result(&own_default)["agentId"], manager["id"]);
    let tasks = call(&s, &configuration, "list_tasks", json!({})).await;
    assert!(
        result(&tasks)["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == foreign["taskId"])
    );
    let runs = call(&s, &configuration, "list_runs", json!({})).await;
    assert!(
        result(&runs)
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == foreign["id"])
    );
    for name in ["get_run", "read_run_content"] {
        let read = call(&s, &configuration, name, json!({ "runId": foreign["id"] })).await;
        result(&read);
    }
    let mut task = result(&created).clone();
    task["name"] = "Updated delegated task".into();
    let updated = call(
        &s,
        &configuration,
        "replace_task",
        json!({
            "taskId": task["id"],
            "revision": task["revision"],
            "settings": mission_settings(&task)
        }),
    )
    .await;
    result(&updated);
    let cancelled = call(
        &s,
        &configuration,
        "cancel_run",
        json!({ "runId": foreign["id"] }),
    )
    .await;
    assert_eq!(result(&cancelled)["cancelled"], true);
    let queued = call(
        &s,
        &configuration,
        "run_task",
        json!({ "taskId": foreign["taskId"] }),
    )
    .await;
    result(&queued);
}

#[tokio::test]
async fn installation_access_can_save_global_and_project_skills() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let manager = s
        .agent_fixture(
            json!({
                "name": "Skill author",
                "access": {
                    "installation": true,
                    "projects": [],
                    "skills": [],
                    }
            }),
            None,
        )
        .await
        .unwrap();
    let (_, configuration) = active_run(&s, &manager).await;
    let project_path = root.path().join("project");
    std::fs::create_dir(&project_path).unwrap();
    let project = s
        .project_fixture(
            json!({
                "name": "Skill project",
                "path": project_path,
                "sourceMode": "local"
            }),
            None,
        )
        .await
        .unwrap();
    let project_id = project["id"].clone();

    let content = "---\nname: independent-review\ndescription: Review until clean.\n---\n\nReview, fix, and repeat.\n";
    for scope in [None, Some(project_id.clone())] {
        let mut args = json!({ "name": "independent-review", "content": content });
        let mut list = json!({});
        if let Some(project_id) = scope {
            args["projectId"] = project_id.clone();
            list["projectId"] = project_id;
        }
        let saved = call(&s, &configuration, "save_skill", args).await;
        result(&saved);
        let skills = call(&s, &configuration, "list_skills", list).await;
        assert!(
            result(&skills)
                .as_array()
                .unwrap()
                .iter()
                .any(|skill| skill["name"] == "independent-review" && skill["content"] == content)
        );
    }

    let invalid = call(
        &s,
        &configuration,
        "save_skill",
        json!({
            "name": "invalid",
            "content": "Missing required frontmatter"
        }),
    )
    .await;
    assert_eq!(invalid["result"]["isError"], true);
    let unknown = call(&s, &configuration, "unknown_tool", json!({})).await;
    assert_eq!(unknown["result"]["isError"], true);
    assert_eq!(unknown["result"]["content"][0]["text"], "Unknown operation");
}

#[tokio::test]
async fn management_discovery_and_legacy_teardown_are_fenced_by_revocation() {
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Duration,
    };
    use tokio::sync::Notify;

    for legacy in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (run, configuration) = active_run(&s, &main).await;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let blocked = Arc::new(AtomicBool::new(false));
        let deletions = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/mcp",
            post({
                let started = started.clone();
                let release = release.clone();
                let blocked = blocked.clone();
                move |Json(request): Json<Value>| {
                    let started = started.clone();
                    let release = release.clone();
                    let blocked = blocked.clone();
                    async move {
                        if legacy && request["method"] == "server/discover" {
                            return StatusCode::BAD_REQUEST.into_response();
                        }
                        if request["method"] == "notifications/initialized" {
                            return StatusCode::ACCEPTED.into_response();
                        }
                        let result = match text(&request, "method") {
                            "server/discover" => json!({
                                "supportedVersions": ["2026-07-28"],
                                "capabilities": {"tools": {}}
                            }),
                            "initialize" => json!({
                                "protocolVersion": "2025-11-25",
                                "capabilities": {"tools": {}}
                            }),
                            method => {
                                assert_eq!(method, "tools/list");
                                if blocked.load(Ordering::SeqCst) {
                                    started.notify_one();
                                    release.notified().await;
                                }
                                json!({"tools": []})
                            }
                        };
                        let mut response = Json(json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": result
                        }))
                        .into_response();
                        if legacy {
                            response
                                .headers_mut()
                                .insert("mcp-session-id", "fixture-session".parse().unwrap());
                        }
                        response
                    }
                }
            })
            .delete({
                let deletions = deletions.clone();
                move || {
                    let deletions = deletions.clone();
                    async move {
                        deletions.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }
            }),
        );
        let (origin, server) = common::serve_locally(app).await;
        let connection = s
            .mcp_fixture(
                json!({
                    "name":"Delayed discovery",
                    "url":format!("{origin}/mcp"),
                    "allowPrivateNetwork":true,
                    "auth":"bearer",
                    "token":"synthetic-teardown-token"
                }),
                None,
            )
            .await
            .unwrap();
        let id = connection["id"].as_str().unwrap();
        // Authorized legacy sessions still send DELETE, and modern ones remain stateless.
        s.mcps.test(&s, id).await.unwrap();
        assert_eq!(deletions.swap(0, Ordering::SeqCst), usize::from(legacy));
        let before = s.mcps.list(&s).await.unwrap();
        blocked.store(true, Ordering::SeqCst);
        let pending = tokio::spawn({
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                call(
                    &s,
                    &configuration,
                    "test_mcp",
                    json!({"id":connection["id"]}),
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(10), started.notified())
            .await
            .unwrap();
        s.mcps
            .revoke_run(&s, run["id"].as_str().unwrap())
            .await
            .unwrap();
        release.notify_one();
        let response = tokio::time::timeout(Duration::from_secs(10), pending)
            .await
            .unwrap()
            .unwrap();
        server.abort();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(
            deletions.load(Ordering::SeqCst),
            0,
            "A revoked grant must not send a remote teardown request"
        );
        assert_eq!(s.mcps.list(&s).await.unwrap(), before);
    }
}

#[tokio::test]
async fn management_oauth_refresh_cannot_save_credentials_after_revocation() {
    use axum::{Json, Router, routing::post};
    use std::time::Duration;
    use tokio::sync::Notify;

    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let app = Router::new().route(
        "/token",
        post({
            let started = started.clone();
            let release = release.clone();
            move || {
                let started = started.clone();
                let release = release.clone();
                async move {
                    started.notify_one();
                    release.notified().await;
                    Json(json!({
                        "access_token": "fixture-refreshed",
                        "token_type": "Bearer",
                        "expires_in": 3600
                    }))
                }
            }
        }),
    );
    let (origin, server) = common::serve_locally(app).await;
    let connection = s
        .mcp_fixture(
            json!({
                "name": "Delayed OAuth refresh",
                "url": format!("{origin}/mcp"),
                "allowPrivateNetwork": true,
                "auth": "oauth",
                "clientId": "fixture-client"
            }),
            None,
        )
        .await
        .unwrap();
    let id = connection["id"].as_str().unwrap().to_owned();
    let credentials = json!({
        "tokens": {
            "access_token": "fixture-expired",
            "refresh_token": "fixture-refresh",
            "token_type": "Bearer"
        },
        "tokenExpiresAt": 1,
        "discovery": {
            "authorizationServerUrl": origin,
            "resourceMetadataUrl": format!("{origin}/resource"),
            "resourceMetadata": { "resource": format!("{origin}/mcp") },
            "authorizationServerMetadata": { "token_endpoint": format!("{origin}/token") }
        }
    });
    let replacement: cairn_installation::mcps::record::McpSecrets =
        serde_json::from_value(credentials.clone()).unwrap();
    s.mcps
        .update_secrets(&s, &id, |secrets| *secrets = replacement)
        .await
        .unwrap();
    let before = s.mcps.list(&s).await.unwrap();
    let pending = tokio::spawn({
        let s = s.clone();
        let configuration = configuration.clone();
        async move {
            call(
                &s,
                &configuration,
                "test_mcp",
                json!({ "id": connection["id"] }),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .unwrap();
    s.mcps
        .revoke_run(&s, run["id"].as_str().unwrap())
        .await
        .unwrap();
    release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    server.abort();
    assert_eq!(response["result"]["isError"], true, "{response}");
    // Assert equality without printing even fixture credentials on failure.
    assert!(
        serde_json::to_value(s.mcps.server_secrets(&s, &id).await.unwrap()).unwrap() == credentials
    );
    assert_eq!(s.mcps.list(&s).await.unwrap(), before);
}

#[tokio::test]
async fn run_grants_enforce_revocation_completion_and_chat_turn_binding() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let id = run["id"].as_str().unwrap();
    s.store
        .update_run_boundary(id, json!({ "chatExecution": { "messageId": "new-turn" } }))
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .update_run_boundary(id, json!({ "chatExecution": null }))
        .await
        .unwrap();
    s.agent_fixture(
        json!({ "name": "Main", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = request(
        &s,
        token,
        "tools/call",
        json!({
            "name": "save_skill",
            "arguments": {
                "name": "revoked",
                "content": "---\nname: revoked\ndescription: Must not be saved.\n---\n"
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!root.path().join("home/.agents/skills/revoked").exists());
    enabled_main(&s).await;
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Running),
                cancel_requested_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.mcps.revoke_run(&s, id).await.unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn workspace_grants_do_not_acquire_installation_access_and_vm_channels_bind_the_run() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let key = hex_digest(token);
    let mut grant = s.store.mcp_run_grant(&key).await.unwrap().unwrap();
    grant.installation = false;
    s.store
        .transaction(move |db| db.save_mcp_run_grant(&key, &grant, None))
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );

    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let home = root.path().join("channel");
    let _channel = channel::serve(&s, &home, run["id"].as_str().unwrap())
        .await
        .unwrap();
    let http = reqwest::Client::builder()
        .unix_socket(home.join("cairn-mcp.sock"))
        .build()
        .unwrap();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    let send = |token: &str| {
        http.post("http://localhost/mcp-installation")
            .bearer_auth(token)
            .json(&body)
    };
    let response = send(token).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response.json::<Value>().await.unwrap()["result"]["tools"].is_array());
    let (_, other) = active_run(&s, &main).await;
    let other_token = other["env"]["CAIRN_MCP_RUN_TOKEN"].as_str().unwrap();
    assert_eq!(
        send(other_token).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
}

async fn public_call(s: &Arc<Service>, scopes: &[&str], name: &str, args: Value) -> Value {
    let args = if name == "save_agent" && args.get("settings").is_none() {
        create_profile(args)
    } else if name == "replace_agent" && args.get("settings").is_none() {
        replacement(s, args).await
    } else {
        args
    };
    let token = common::relay_fixture::grant(s, scopes).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let request = common::request("POST", "/mcp")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": common::mcp_arguments(name, &args),
            })
            .to_string(),
        ))
        .unwrap();
    let response = common::send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    common::read_json(response).await
}

#[tokio::test]
async fn mission_conversion_matches_public_mcp_and_installation_tools() {
    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (caller, configuration) = active_run(&s, &main).await;
        let (target, _) = active_run(&s, &main).await;
        let run_id = target["id"].as_str().unwrap();
        s.store
            .change_run(
                run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Succeeded),
                    session_id: Some(Some("mission-session".into())),
                    workspace: Some(Some("/saved/mission-workspace".into())),
                    summary: Some(("Mission result").to_owned()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let original = s.store.run(run_id).await.unwrap();
        s.store
            .save_checkpoint_boundary(
                run_id,
                json!({
                    "prepared": {}, "completed": true,
                }),
            )
            .await
            .unwrap();
        let invoke = |name, args| {
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                if internal {
                    call(&s, &configuration, name, args).await
                } else {
                    public_call(&s, &["manage", "run"], name, args).await
                }
            }
        };

        let discovery = invoke(
            "search",
            json!({
                "names": ["convert_run_to_chat"], "detail": "schema",
            }),
        )
        .await;
        assert_eq!(
            result(&discovery)["operations"][0]["name"],
            "convert_run_to_chat"
        );
        let args = json!({ "runId": run_id });
        for scopes in [&["read"][..], &["run"][..]] {
            let denied = public_call(&s, scopes, "convert_run_to_chat", args.clone()).await;
            assert_eq!(denied["result"]["isError"], true);
            assert!(s.chat_list().await.unwrap().is_empty());
            assert_eq!(s.store.run(run_id).await.unwrap(), original);
        }

        let converted = invoke("convert_run_to_chat", args.clone()).await;
        let chat = result(&converted);
        let retried = invoke("convert_run_to_chat", args.clone()).await;
        assert_eq!(result(&retried)["id"], chat["id"]);
        assert_eq!(chat["runId"], original["id"]);
        let converted_run = s.store.run(run_id).await.unwrap();
        for field in ["id", "sessionId", "workspace", "summary", "status"] {
            assert_eq!(converted_run[field], original[field], "{field}");
        }
        assert_eq!(converted_run["sourceTaskId"], original["taskId"]);
        assert_eq!(s.chat_list().await.unwrap().len(), 1);
        let message = invoke(
            "send_chat_message",
            json!({
                "chatId": chat["id"],
                "message": { "id": uuid::Uuid::new_v4(), "text": "Explain the mission result" },
            }),
        )
        .await;
        assert_eq!(result(&message)["status"], "queued");

        if internal {
            s.mcps
                .revoke_run(&s, caller["id"].as_str().unwrap())
                .await
                .unwrap();
            let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
                .as_str()
                .unwrap();
            let (status, _) = request(
                &s,
                token,
                "tools/call",
                common::mcp_arguments("convert_run_to_chat", &args),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
    }
}

#[tokio::test]
async fn plugin_setup_is_available_on_both_endpoints_without_granting_private_accounts() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let saved = common::save_plugin(
        &s,
        "onepassword",
        &json!({
            "name": "Private", "enabled": true, "secrets": { "token": "ops_fixture_private" },
        }),
        None,
    )
    .await
    .unwrap();
    for internal in [false, true] {
        let list = if internal {
            call(&s, &configuration, "list_plugins", json!({})).await
        } else {
            public_call(&s, &["read"], "list_plugins", json!({})).await
        };
        assert!(
            result(&list)["packages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == "gmail")
        );
        assert!(!list.to_string().contains("ops_fixture_private"));
        let args = json!({ "pluginId": "gmail", "kind": "setup" });
        let setup = if internal {
            call(&s, &configuration, "manage_plugin", args).await
        } else {
            public_call(&s, &["manage"], "manage_plugin", args).await
        };
        assert!(
            result(&setup)["callbackUrl"]
                .as_str()
                .unwrap()
                .ends_with("/plugins/oauth/callback")
        );
        let forbidden = json!({
            "pluginId": "onepassword",
            "kind": "save",
            "instanceId": saved["id"],
            "data": { "agentIds": [MAIN_AGENT_ID] },
        });
        let denied = if internal {
            call(&s, &configuration, "manage_plugin", forbidden).await
        } else {
            public_call(&s, &["manage"], "manage_plugin", forbidden).await
        };
        assert_eq!(denied["result"]["isError"], true, "{denied}");
    }
    let denied = call(
        &s,
        &configuration,
        "manage_plugin",
        json!({ "pluginId": "onepassword", "kind": "view", "instanceId": saved["id"] }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let bypass = call(
        &s,
        &configuration,
        "manage_plugin",
        json!({ "pluginId": "onepassword", "kind": "form", "data": { "instance": saved["id"] } }),
    )
    .await;
    assert_eq!(bypass["result"]["isError"], true, "{bypass}");
    assert_eq!(
        s.instance_fixture(saved["id"].as_str().unwrap())
            .await
            .unwrap()["agentIds"],
        json!([])
    );
}

#[tokio::test]
async fn both_mcp_endpoints_can_pause_and_delete_instances_pending_package_reapproval() {
    use cairn_installation::{
        plugins::{connections, registry},
        validation::text,
    };
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let installed = common::package_install(
        &s,
        &json!({
            "manifest": {
                "id": "pausable", "name": "Pausable", "version": "1.0.0",
                "apiVersion": 2, "capabilities": [],
            },
            "source": "return {}",
        }),
    )
    .await
    .unwrap();
    common::package_approve(&s, "pausable", "1.0.0", text(&installed, "digest"))
        .await
        .unwrap();
    let saved = common::save_plugin(
        &s,
        "pausable",
        &json!({
            "name": "Pause me", "enabled": true,
        }),
        None,
    )
    .await
    .unwrap();
    std::fs::write(
        root.path().join("data/plugins/pausable/1.0.0/main.luau"),
        "return {} -- changed after approval",
    )
    .unwrap();
    assert!(registry::active(&s, "pausable").await.is_err());
    for internal in [false, true] {
        let args = json!({
            "pluginId": "pausable", "kind": "enabled", "instanceId": saved["id"],
            "data": { "enabled": false },
        });
        let paused = if internal {
            call(&s, &configuration, "manage_plugin", args).await
        } else {
            public_call(&s, &["manage"], "manage_plugin", args).await
        };
        assert_eq!(result(&paused)["instance"]["enabled"], false, "{paused}");
    }
    let removed = call(
        &s,
        &configuration,
        "manage_plugin",
        json!({
            "pluginId": "pausable", "kind": "delete", "instanceId": saved["id"],
        }),
    )
    .await;
    assert_ne!(removed["result"]["isError"], true, "{removed}");
    assert!(
        connections::get(&s, "pausable", text(&saved, "id"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn both_mcp_endpoints_list_current_chat_account_waits() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (target, configuration) = active_run(&s, &main).await;
    let mut chat = s.chat_create(json!({})).await.unwrap();
    chat["runId"] = target["id"].clone();
    s.store.put("chats", chat.clone()).await.unwrap();
    for required in [None, Some(cairn_contracts::AgentProvider::Codex)] {
        s.store
            .change_run(
                text(&target, "id"),
                cairn_installation::runs::RunChanges {
                    account_wait_reason: Some(Some(
                        "Waiting for a Codex account with available usage.".into(),
                    )),
                    account_required: Some(required),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
        for internal in [false, true] {
            let response = if internal {
                call(&s, &configuration, "list_chats", json!({})).await
            } else {
                public_call(&s, &["read"], "list_chats", json!({})).await
            };
            let listed = &result(&response)["chats"][0];
            assert_eq!(
                listed["accountWaitReason"],
                detail["run"]["accountWaitReason"]
            );
            assert_eq!(listed["accountRequired"], detail["run"]["accountRequired"]);
            assert_eq!(listed["accountWaitReason"], detail["accountWaitReason"]);
            assert_eq!(listed["accountRequired"], detail["accountRequired"]);
            assert!(listed.get("run").is_none());
        }
    }
    s.store
        .change_run(
            text(&target, "id"),
            cairn_installation::runs::RunChanges {
                account_wait_reason: Some(None),
                account_required: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let cleared = s.chat_detail(text(&chat, "id")).await.unwrap();
    assert!(cleared["accountWaitReason"].is_null());
    assert!(cleared["accountRequired"].is_null());
}

#[tokio::test]
async fn both_mcp_endpoints_manage_chat_queues_and_lifecycle() {
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
        assert_eq!(result(&created)["agentId"], MAIN_AGENT_ID);
        let pause = invoke(
            "set_chat_paused",
            json!({ "chatId": chat_id, "paused": true }),
        )
        .await;
        assert_eq!(result(&pause)["paused"], true);
        let message_id = uuid::Uuid::new_v4().to_string();
        let args = json!({
            "chatId": chat_id,
            "message": { "id": message_id, "text": "First message" },
        });
        let sent = invoke("send_chat_message", args.clone()).await;
        assert_eq!(result(&sent)["status"], "queued");
        let retry = invoke("send_chat_message", args).await;
        assert_eq!(result(&sent), result(&retry));
        let changed = invoke(
            "edit_chat_message",
            json!({
                "chatId": chat_id, "messageId": message_id, "message": { "text": "Edited" },
            }),
        )
        .await;
        assert_eq!(result(&changed)["text"], "Edited");
        let renamed = invoke(
            "rename_chat",
            json!({ "chatId": chat_id, "title": "  Gestion Cairn  " }),
        )
        .await;
        assert_eq!(result(&renamed)["title"], "Gestion Cairn");
        assert_eq!(
            invoke("rename_chat", json!({ "chatId": chat_id, "title": "   " })).await["result"]["isError"],
            true
        );
        let listed = invoke("list_chats", json!({ "limit": 1 })).await;
        assert_eq!(result(&listed)["chats"][0]["id"], chat_id);
        let detail = invoke("get_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&detail)["messages"][0]["text"], "Edited");
        assert!(result(&detail)["chat"].get("run").is_none());
        let resumed = invoke(
            "set_chat_paused",
            json!({ "chatId": chat_id, "paused": false }),
        )
        .await;
        assert_eq!(result(&resumed)["paused"], false);
        let stopped = invoke("stop_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&stopped)["paused"], true);
        assert_eq!(
            invoke("delete_chat", json!({ "chatId": chat_id })).await["result"]["isError"],
            true
        );
        let removed = invoke(
            "delete_chat_message",
            json!({ "chatId": chat_id, "messageId": message_id }),
        )
        .await;
        assert_eq!(result(&removed)["deleted"], true);
        let deleted = invoke("delete_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&deleted)["lifecycle"], "trash");
        let listed = invoke("list_chats", json!({ "view": "trash" })).await;
        assert_eq!(result(&listed)["chats"][0]["id"], chat_id);
        let rejected = invoke(
            "send_chat_message",
            json!({
                "chatId": chat_id, "message": { "id": uuid::Uuid::new_v4(), "text": "Deleted" },
            }),
        )
        .await;
        assert_eq!(rejected["result"]["isError"], true);
    }
}

#[tokio::test]
async fn chat_tools_preserve_scopes_caller_defaults_and_revocation() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let agent = s
        .agent_fixture(
            json!({
                "name": "Delegated manager", "access": { "installation": true, "projects": [] },
            }),
            None,
        )
        .await
        .unwrap();
    let (run, configuration) = active_run(&s, &agent).await;
    let created = call(&s, &configuration, "create_chat", json!({})).await;
    let chat_id = result(&created)["id"].as_str().unwrap();
    assert_eq!(result(&created)["agentId"], agent["id"]);
    for (name, args) in [
        ("create_chat", json!({})),
        (
            "rename_chat",
            json!({ "chatId": chat_id, "title": "Denied" }),
        ),
        (
            "send_chat_message",
            json!({ "chatId": chat_id, "message": { "id": uuid::Uuid::new_v4(), "text": "Denied" } }),
        ),
        ("delete_chat", json!({ "chatId": chat_id })),
    ] {
        let denied = public_call(&s, &["read"], name, args).await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
        assert!(denied["result"]["_meta"]["mcp/www_authenticate"].is_string());
    }
    let read = public_call(&s, &["read"], "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&read)["chat"]["id"], chat_id);
    s.store
        .change_run(
            run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    for name in [
        "list_chats",
        "create_chat",
        "get_chat",
        "list_chat_artifacts",
        "stop_chat",
    ] {
        assert_eq!(
            request(
                &s,
                token,
                "tools/call",
                json!({ "name": name, "arguments": {} })
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn chat_file_tools_read_private_artifacts_and_attachments_in_bounded_chunks() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let (target, _) = active_run(&s, &main).await;
    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let mut chat = chat.clone();
    chat["runId"] = target["id"].clone();
    s.store.put("chats", chat).await.unwrap();
    let artifact_id = uuid::Uuid::new_v4().to_string();
    let data = (0..220_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let digest = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&data))
    };
    let record = json!({
        "id": artifact_id, "runId": target["id"], "key": "report", "version": 1,
        "title": "Private report", "name": "report.bin", "size": data.len(),
        "digest": digest, "visibility": "private", "createdAt": 1,
    });
    s.store
        .set(
            &format!("artifact:{}:{artifact_id}", target["id"].as_str().unwrap()),
            record,
            None,
        )
        .await
        .unwrap();
    tokio::fs::create_dir_all(s.config.data_dir.join("artifacts"))
        .await
        .unwrap();
    tokio::fs::write(
        s.config.data_dir.join("artifacts").join(&artifact_id),
        &data,
    )
    .await
    .unwrap();
    let listed = call(
        &s,
        &configuration,
        "list_chat_artifacts",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&listed)["artifacts"][0]["visibility"], "private");
    let public_file = public_call(
        &s,
        &["read"],
        "read_chat_artifact",
        json!({
            "chatId": chat_id, "artifactId": artifact_id,
        }),
    )
    .await;
    assert_eq!(result(&public_file)["totalBytes"], data.len());
    let denied_share = public_call(
        &s,
        &["read"],
        "set_chat_artifact_visibility",
        json!({
            "chatId": chat_id, "artifactId": artifact_id, "visibility": "public",
        }),
    )
    .await;
    assert_eq!(denied_share["result"]["isError"], true);
    let mut offset = 0;
    let mut received = Vec::new();
    loop {
        let chunk = call(
            &s,
            &configuration,
            "read_chat_artifact",
            json!({
                "chatId": chat_id, "artifactId": artifact_id, "offset": offset,
            }),
        )
        .await;
        let chunk = result(&chunk);
        let bytes = STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap();
        assert!(bytes.len() <= 192 * 1024);
        received.extend(bytes);
        let Some(next) = chunk["nextOffset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(received, data);
    let shared = call(
        &s,
        &configuration,
        "set_chat_artifact_visibility",
        json!({
            "chatId": chat_id, "artifactId": artifact_id, "visibility": "public",
        }),
    )
    .await;
    assert!(
        result(&shared)["publicUrl"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
    let private = call(
        &s,
        &configuration,
        "set_chat_artifact_visibility",
        json!({
            "chatId": chat_id, "artifactId": artifact_id, "visibility": "private",
        }),
    )
    .await;
    assert!(result(&private)["publicUrl"].is_null());
    let other = s.chat_create(json!({})).await.unwrap();
    let denied = call(
        &s,
        &configuration,
        "read_chat_artifact",
        json!({
            "chatId": other["id"], "artifactId": artifact_id,
        }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    let attachment_id = uuid::Uuid::new_v4().to_string();
    let upload = json!({
        "chatId": chat_id, "attachmentId": attachment_id, "name": "note.txt",
        "data": STANDARD.encode("Bonjour à tous"),
    });
    let uploaded = call(&s, &configuration, "upload_chat_attachment", upload.clone()).await;
    let retried = call(&s, &configuration, "upload_chat_attachment", upload).await;
    assert_eq!(result(&uploaded), result(&retried));
    let attachments = call(
        &s,
        &configuration,
        "list_chat_attachments",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&attachments)["attachments"][0]["id"], attachment_id);
    let downloaded = call(
        &s,
        &configuration,
        "read_chat_attachment",
        json!({ "chatId": chat_id, "attachmentId": attachment_id }),
    )
    .await;
    assert_eq!(
        STANDARD
            .decode(result(&downloaded)["data"].as_str().unwrap())
            .unwrap(),
        "Bonjour à tous".as_bytes()
    );
    let stopped = call(
        &s,
        &configuration,
        "stop_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&stopped)["paused"], true);
    assert_eq!(
        s.store.run(target["id"].as_str().unwrap()).await.unwrap()["status"],
        RunStatus::Cancelled
    );
}

#[tokio::test]
async fn chat_questions_redact_private_answers_and_content_chunks_detect_changes() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let (target, _) = active_run(&s, &main).await;
    let mut chat = s.chat_create(json!({})).await.unwrap();
    chat["runId"] = target["id"].clone();
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    s.store.put("chats", chat).await.unwrap();
    let question_id = "a".repeat(64);
    s.question_receive(
        target["id"].as_str().unwrap(),
        json!({
            "id": question_id, "blocking": true,
            "fields": [{ "id": "secret", "title": "Private value", "secret": true }],
        }),
    )
    .await
    .unwrap();
    let questions = call(
        &s,
        &configuration,
        "list_chat_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&questions)["questions"][0]["status"], "pending");
    let answer = json!({
        "chatId": chat_id, "questionId": question_id,
        "answer": { "id": uuid::Uuid::new_v4(), "answers": { "secret": ["fixture-private-answer"] } },
    });
    let answered = call(&s, &configuration, "answer_chat_question", answer.clone()).await;
    assert_eq!(result(&answered)["status"], "answering");
    let repeated = call(&s, &configuration, "answer_chat_question", answer).await;
    assert_eq!(result(&answered), result(&repeated));
    for tool in ["get_chat", "read_chat_content", "list_chat_questions"] {
        let response = call(&s, &configuration, tool, json!({ "chatId": chat_id })).await;
        result(&response);
        assert!(
            !response.to_string().contains("fixture-private-answer"),
            "{tool} exposed a private answer"
        );
    }
    // Populate a long, entirely delivered history so pagination is limited by bytes,
    // independently of the undelivered-message queue limit.
    let chat_key = chat_id.clone();
    s.store
        .transaction(move |db| {
            for i in 0..8 {
                db.put_message(&json!({
                    "id": uuid::Uuid::new_v4(), "chatId": chat_key,
                    "text": "é".repeat(50_000), "status": "delivered", "createdAt": i,
                }))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let first = call(&s, &configuration, "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&first)["hasMore"], true);
    let mut offset = 0;
    let mut messages = Vec::new();
    loop {
        let response = call(
            &s,
            &configuration,
            "get_chat",
            json!({ "chatId": chat_id, "offset": offset }),
        )
        .await;
        messages.extend(result(&response)["messages"].as_array().unwrap().clone());
        let Some(next) = result(&response)["nextOffset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(messages.len(), 9);
    assert_eq!(messages[8]["text"], "Private answer");
    let mut offset = 0;
    let mut checksum = Value::Null;
    let mut content = String::new();
    loop {
        let mut args = json!({ "chatId": chat_id, "offset": offset });
        if !checksum.is_null() {
            args["sha256"] = checksum.clone();
        }
        let response = call(&s, &configuration, "read_chat_content", args).await;
        let chunk = result(&response);
        checksum = chunk["sha256"].clone();
        content.push_str(chunk["data"].as_str().unwrap());
        let Some(next) = chunk["nextOffset"].as_u64() else {
            break;
        };
        offset = next;
    }
    let decoded: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(decoded["messages"].as_array().unwrap().len(), 9);
    let renamed = call(
        &s,
        &configuration,
        "rename_chat",
        json!({ "chatId": chat_id, "title": "Changed snapshot" }),
    )
    .await;
    result(&renamed);
    let stale = call(
        &s,
        &configuration,
        "read_chat_content",
        json!({ "chatId": chat_id, "offset": 1, "sha256": checksum }),
    )
    .await;
    assert_eq!(stale["result"]["isError"], true);
}

#[tokio::test]
async fn chat_recovery_tools_preserve_confirmation_and_retention_rules() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let (target, _) = active_run(&s, &main).await;
    let run_id = target["id"].as_str().unwrap();
    s.store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                boundary: Some(serde_json::json!({"restoredAt": 1})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.store
        .save_checkpoint_boundary(run_id, json!({ "launched": true }))
        .await
        .unwrap();
    let mut chat = s.chat_create(json!({})).await.unwrap();
    chat["runId"] = target["id"].clone();
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    s.store.put("chats", chat).await.unwrap();
    let denied = call(
        &s,
        &configuration,
        "restart_chat_session",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    let restarted = call(
        &s,
        &configuration,
        "restart_chat_session",
        json!({ "chatId": chat_id, "confirm": true }),
    )
    .await;
    assert_eq!(result(&restarted)["sessionRestartRequested"], true);
    assert_eq!(result(&restarted)["paused"], true);
    let replay = call(
        &s,
        &configuration,
        "resume_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(replay["result"]["isError"], true);
    let deleted = call(
        &s,
        &configuration,
        "delete_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&deleted)["lifecycle"], "trash");
    let expired = call(
        &s,
        &configuration,
        "restore_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(expired["result"]["isError"], true);
    let mut retained = s.get("chats", &chat_id).await.unwrap();
    retained["purgeAt"] = (cairn_installation::config::now() + 60_000).into();
    s.store.put("chats", retained).await.unwrap();
    let restored = call(
        &s,
        &configuration,
        "restore_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&restored)["lifecycle"], "active");
    assert_eq!(result(&restored)["paused"], true);
}

#[tokio::test]
async fn code_mode_discovers_schemas_and_composes_chat_and_artifact_reads() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let summary = call(
        &s,
        &configuration,
        "search",
        json!({ "query": "artifact", "limit": 20 }),
    )
    .await;
    let operations = result(&summary)["operations"].as_array().unwrap();
    assert!(
        operations
            .iter()
            .any(|op| op["name"] == "read_chat_artifact")
    );
    assert!(operations.iter().all(|op| op.get("inputSchema").is_none()));
    let schema = call(
        &s,
        &configuration,
        "search",
        json!({
            "names": ["create_chat"], "detail": "schema",
        }),
    )
    .await;
    assert_eq!(
        result(&schema)["operations"][0]["inputSchema"]["properties"]["agentId"]["default"],
        MAIN_AGENT_ID
    );
    let response = call(&s, &configuration, "execute", json!({
        "input": { "text": "A message containing quotes: \" and ` ${input} ", "id": uuid::Uuid::new_v4() },
        "code": r#"
            const chat = await cairn.create_chat({});
            await cairn.set_chat_paused({chatId: chat.id, paused: true});
            await cairn.send_chat_message({chatId: chat.id, message: input});
            const detail = await cairn.get_chat({chatId: chat.id});
            const files = await cairn.list_chat_artifacts({chatId: chat.id});
            return {id: chat.id, text: detail.messages[0].text, files: files.artifacts.length};
        "#,
    })).await;
    assert_eq!(
        result(&response)["text"],
        "A message containing quotes: \" and ` ${input}"
    );
    assert_eq!(result(&response)["files"], 0);
    assert!(result(&response)["id"].is_string());
}

#[tokio::test]
async fn code_mode_checks_each_operations_scopes_and_revocation_without_rolling_back_prior_effects()
{
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let denied = public_call(
        &s,
        &["read", "manage"],
        "execute",
        json!({
            "code": r#"
            const chat = await cairn.create_chat({});
            await cairn.send_chat_message({chatId: chat.id, message: input});
            return chat;
        "#,
            "input": { "id": uuid::Uuid::new_v4(), "text": "Must not execute" },
        }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(
        denied["result"]["_meta"]["mcp/www_authenticate"]
            .as_str()
            .unwrap()
            .contains("run")
    );
    let listed = public_call(&s, &["read"], "list_chats", json!({})).await;
    let chat_id = &result(&listed)["chats"][0]["id"];
    let detail = public_call(&s, &["read"], "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&detail)["messages"], json!([]));

    let mut access = main["access"].clone();
    access["installation"] = false.into();
    let revoked = call(
        &s,
        &configuration,
        "execute",
        json!({
            "code": r#"
            await cairn.replace_agent(input);
            return await cairn.list_chats({});
        "#,
            "input": replacement(&s, json!({ "id": MAIN_AGENT_ID, "agent": { "access": access } })).await,
        }),
    )
    .await;
    assert_eq!(revoked["result"]["isError"], true);
    let agents = public_call(&s, &["read"], "list_agents", json!({})).await;
    let main = result(&agents)["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == MAIN_AGENT_ID)
        .unwrap();
    assert_eq!(main["access"]["installation"], false);
}

#[tokio::test]
async fn code_mode_isolates_scripts_bounds_resources_and_remains_usable_after_failure() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let isolated = call(&s, &configuration, "execute", json!({
        "code": "globalThis.persisted = 1; return { frozen: Object.isFrozen(cairn), unavailable: [typeof process, typeof require, typeof fetch, typeof __cairnHost, typeof __cairnInput] };",
    })).await;
    assert_eq!(result(&isolated)["frozen"], true);
    assert_eq!(
        result(&isolated)["unavailable"],
        json!([
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined"
        ])
    );
    let fresh = call(
        &s,
        &configuration,
        "execute",
        json!({ "code": "return typeof persisted;" }),
    )
    .await;
    assert_eq!(result(&fresh), "undefined");
    for code in [
        "return (", // Syntax errors cannot reach an operation.
        "return await import('node:fs');",
        "return () => 42;",
        "return 'x'.repeat(1100000);",          // Result budget.
        "return 'x'.repeat(80 * 1024 * 1024);", // Heap budget.
        "for (;;) {}",                          // Process CPU budget.
        "for (let i = 0; i < 65; i++) await cairn.list_chats({}); return true;",
    ] {
        let failed = call(&s, &configuration, "execute", json!({ "code": code })).await;
        assert_eq!(failed["result"]["isError"], true, "{code}: {failed}");
    }
    let invalid = call(
        &s,
        &configuration,
        "execute",
        json!({
            "code": "return await cairn.create_chat({agentId: 'invalid'});",
        }),
    )
    .await;
    assert_eq!(invalid["result"]["isError"], true);
    let valid = call(
        &s,
        &configuration,
        "execute",
        json!({ "code": "return input.value + 1;", "input": { "value": 41 } }),
    )
    .await;
    assert_eq!(result(&valid), 42);
}

#[tokio::test]
async fn installation_access_cannot_grant_secrets_or_exclusive_reservations() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let account = common::save_plugin(
        &s,
        "onepassword",
        &json!({ "name": "Production", "enabled": true, "secrets": { "token": "ops_fixture" } }),
        None,
    )
    .await
    .unwrap();
    let account = account["id"].clone();
    let manager = s
        .agent_fixture(
            json!({
                "name": "Manager",
                "access": { "installation": true },
                "plugins": [account]
            }),
            None,
        )
        .await
        .unwrap();
    let (_, configuration) = active_run(&s, &manager).await;

    // Neither an update nor a new agent can change the owner's live grants.
    call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": manager["id"],
            "agent": { "description": "Edited", "plugins": [], "exclusiveNodes": [] }
        }),
    )
    .await;
    let stored = s
        .agent_snapshot(manager["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(stored["plugins"], json!([account]));

    let created = call(
        &s,
        &configuration,
        "save_agent",
        json!({ "name": "Escalation", "plugins": [account] }),
    )
    .await;
    // The published contract has no live grants: the request is refused or ignored.
    let agents = s.store.agent_snapshots().await.unwrap();
    for agent in agents.iter().filter(|agent| agent["id"] != manager["id"]) {
        assert_eq!(agent["plugins"], json!([]), "{created}");
    }
    let instance = s
        .store
        .instance_fixture(account.as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance["agentIds"], json!([manager["id"]]));
}

#[tokio::test]
async fn backend_migration_tools_match_and_recheck_management_grants() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let chat = cairn_installation::config::id();
    let run = cairn_installation::config::id();
    let node = cairn_installation::config::id();
    s.store
        .put(
            "nodes",
            json!({
                "id": node,
                "runtimeId": "development",
                "capabilities": { "backend": "native" },
                "nativeEnabled": true,
                "nativeAccounts": [],
            }),
        )
        .await
        .unwrap();
    s.store
        .set(
            "native-authority",
            json!({ "observedAt": cairn_installation::config::now(), "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    // Neither caller has a traced native host grant. Management access alone
    // must not allow the migration to manufacture that authority.
    let profile = s.agent_fixture(json!({ "name": "Native destination", "access": { "backend": "native", "nodes": [node] } }), None).await.unwrap();
    let mut previous = profile.clone();
    previous["access"]["backend"] = "firecracker".into();
    common::add_run(
        &s.store,
        &json!({
            "id": run,
            "taskId": "fixture",
            "status": "succeeded",
            "createdAt": 0,
            "snapshot": { "agent": previous },
        }),
    )
    .await;
    s.store
        .put(
            "chats",
            json!({ "id": chat, "runId": run, "agentId": profile["id"] }),
        )
        .await
        .unwrap();
    common::set_checkpoint(
        &s.store,
        &run,
        json!({ "prepared": { "projectRoot": "/data/work" } }),
    )
    .await;
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, preview) = node_management_call(
            &app,
            endpoint,
            token,
            "get_chat_backend_migration",
            json!({ "chatId": chat }),
        )
        .await;
        assert_eq!(result(&preview)["required"], true);
        assert!(result(&preview)["nodes"].as_array().unwrap().is_empty());
        let (_, denied) = node_management_call(
            &app,
            endpoint,
            token,
            "migrate_chat_backend",
            json!({ "chatId": chat, "nodeId": node, "backend": "native", "confirm": true }),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
        assert!(s.store.run(&run).await.unwrap()["backendMigration"].is_null());
    }
    let read = common::relay_fixture::grant(&s, &["read"]).await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read,
        "migrate_chat_backend",
        json!({ "chatId": chat, "nodeId": node, "backend": "native", "confirm": true }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(denied["result"]["_meta"]["mcp/www_authenticate"].is_string());
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let (status, _) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "get_chat_backend_migration",
        json!({ "chatId": chat }),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
}

#[tokio::test]
async fn waiting_missions_are_visible_and_cancellable_through_both_installation_interfaces() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage", "run"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (run, _) = active_run(&s, &main).await;
        let run_id = run["id"].as_str().unwrap();
        let wakeup = json!({
            "id": cairn_installation::config::id(),
            "messageId": null,
            "reason": "Mission CI",
            "seconds": 60,
            "createdAt": 1,
            "wakeAt": 60_001,
        });
        s.store
            .change_run(
                run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Waiting),
                    boundary: Some(serde_json::json!({"wakeup": wakeup})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (status, fetched) =
            node_management_call(&app, endpoint, token, "get_run", json!({ "runId": run_id }))
                .await;
        assert_eq!(status, StatusCode::OK, "{fetched}");
        assert_eq!(
            result(&fetched)["run"]["run"]["status"],
            "waiting",
            "{fetched}"
        );
        assert_eq!(result(&fetched)["run"]["run"]["wakeup"], wakeup);
        let (_, listed) = node_management_call(
            &app,
            endpoint,
            token,
            "list_runs",
            json!({ "status": "waiting" }),
        )
        .await;
        assert!(
            result(&listed)
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == run_id && item["wakeup"] == wakeup)
        );
        let (_, cancelled) = node_management_call(
            &app,
            endpoint,
            token,
            "cancel_run",
            json!({ "runId": run_id }),
        )
        .await;
        assert_eq!(result(&cancelled)["cancelled"], true);
        let final_run = s.store.run(run_id).await.unwrap();
        assert_eq!(final_run["status"], "cancelled");
        assert!(final_run["wakeup"].is_null());
    }
}

#[tokio::test]
async fn neither_agent_interface_can_review_an_administrator_node() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let agent = s
        .agent_fixture(
            json!({ "name": "Wildcard native", "access": { "backend": "native", "nodes": null } }),
            None,
        )
        .await
        .unwrap();
    let mut nodes = Vec::new();
    for _ in 0..2 {
        let node = cairn_installation::config::id();
        s.store.put("nodes", json!({
            "id": node, "capabilities": { "backend": "native", "nativeModeProtocol": 1 },
            "nativeModeRequest": { "mode": "administrator", "generation": cairn_installation::config::id() },
            "nativeReviewedAgents": [],
        })).await.unwrap();
        nodes.push(node);
    }
    let args = replacement(
        &s,
        json!({ "id": agent["id"], "agent": { "nativeNodeReviews": [nodes[0]] } }),
    )
    .await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "replace_agent",
        args.clone(),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert!(
        s.get("nodes", &nodes[0]).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (_, response) =
        node_management_call(&app, "/mcp", &owner_token, "replace_agent", args).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(
        s.get("nodes", &nodes[0]).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn chat_compaction_matches_public_mcp_and_installation_tools() {
    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (_, configuration) = active_run(&s, &main).await;
        let (target, _) = active_run(&s, &main).await;
        let run_id = target["id"].as_str().unwrap();
        s.store
            .change_run(
                run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_installation::run_status::RunStatus::Succeeded),
                    session_id: Some(Some("native-session".into())),
                    summary: Some("Last reply".into()),
                    finished_at: Some(Some(500)),
                    boundary: Some(json!({
                        "contextUsage": {
                            "contextTokens": 150_000,
                            "contextWindow": 200_000,
                            "cachedTokens": 120_000,
                            "estimated": false,
                        },
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        s.store
            .save_checkpoint_boundary(run_id, json!({ "launched": true, "completed": true }))
            .await
            .unwrap();
        let mut chat = s.chat_create(json!({})).await.unwrap();
        chat["runId"] = target["id"].clone();
        let chat_id = chat["id"].as_str().unwrap().to_owned();
        s.store.put("chats", chat).await.unwrap();
        let invoke = |name, args| {
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                if internal {
                    call(&s, &configuration, name, args).await
                } else {
                    public_call(&s, &["read", "run"], name, args).await
                }
            }
        };

        let metadata = invoke("get_run", json!({ "runId": run_id })).await;
        assert_eq!(
            result(&metadata)["run"]["run"]["contextUsage"]["contextTokens"],
            150_000
        );

        let read = invoke("get_chat", json!({ "chatId": chat_id })).await;
        let summary = &result(&read)["chat"];
        assert_eq!(summary["contextUsage"]["contextTokens"], 150_000);
        assert_eq!(summary["compaction"]["available"], true);
        assert_eq!(summary["compaction"]["supported"], true);

        // Compaction starts model work: reading alone is not enough.
        let denied = public_call(&s, &["read"], "compact_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(denied["result"]["isError"], true);
        assert_eq!(s.store.run(run_id).await.unwrap()["status"], "succeeded");

        // The turn's worker still owns the run while it saves its checkpoint: compaction
        // waits for it rather than resetting that checkpoint underneath it.
        s.worker.active.lock().await.insert(
            run_id.to_owned(),
            tokio_util::sync::CancellationToken::new(),
        );
        let busy = invoke("compact_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(busy["result"]["isError"], true, "{busy}");
        assert!(
            busy.to_string().contains("Wait for the current turn"),
            "{busy}"
        );
        let untouched = s.store.run(run_id).await.unwrap();
        assert_eq!(untouched["status"], "succeeded");
        assert!(untouched["chatExecution"]["operation"].is_null());
        s.worker.active.lock().await.remove(run_id);

        let compacted = invoke("compact_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&compacted)["compaction"]["running"], true);
        let run = s.store.run(run_id).await.unwrap();
        assert_eq!(run["status"], "queued");
        assert_eq!(run["chatExecution"]["operation"], "compact");
        assert_eq!(run["summary"], "Last reply");

        // A retry keeps the same compaction; nothing else can start meanwhile.
        let retried = invoke("compact_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&retried)["compaction"]["running"], true);
        assert_eq!(
            s.store.run(run_id).await.unwrap()["chatExecution"],
            run["chatExecution"]
        );
    }
}

async fn saved_mission(s: &Service, agent: &Value) -> Value {
    let (run, _) = active_run(s, agent).await;
    let id = text(&run, "id");
    common::set_checkpoint(
        &s.store,
        id,
        json!({
            "launched": true,
            "prepared": { "cwd": "/saved/workspace", "marker": "retained" },
            "generation": "retained-generation",
            "remainingMs": 0,
        }),
    )
    .await;
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Failed),
                session_id: Some(Some("saved-provider-session".into())),
                workspace: Some(Some("/saved/workspace".into())),
                resume_available: Some(true),
                retry: Some(Some(serde_json::from_value(
                    serde_json::json!({ "attempt": 5, "limit": 5, "cause": {"kind":"timeout", "message":"Temporary timeout"} }),
                ).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.store.run(id).await.unwrap()
}

#[tokio::test]
async fn resume_run_is_one_capability_on_http_public_and_installation() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let public = common::relay_fixture::grant(&s, &["run"]).await;
    let read = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);
    let member =
        common::RelayContext::new(&common::relay_fixture::member_context(&s, "other-member").await);
    let agent_token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    for surface in ["owner", "member", "public", "installation"] {
        let before = saved_mission(&s, &main).await;
        let id = text(&before, "id");
        let (_, denied) =
            node_management_call(&app, "/mcp", &read, "resume_run", json!({ "runId": id })).await;
        assert_eq!(denied["result"]["isError"], true);
        assert!(denied["result"]["_meta"]["mcp/www_authenticate"].is_string());
        assert_eq!(s.store.run(id).await.unwrap(), before);
        let output = if matches!(surface, "owner" | "member") {
            let context = if surface == "owner" { &owner } else { &member };
            let request = context
                .authorize(common::request("POST", &format!("/api/runs/{id}/resume")))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let response = common::send(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            common::read_json(response).await
        } else {
            let (endpoint, token) = if surface == "public" {
                ("/mcp", public.as_str())
            } else {
                ("/mcp-installation", agent_token)
            };
            let (_, response) =
                node_management_call(&app, endpoint, token, "resume_run", json!({ "runId": id }))
                    .await;
            result(&response).clone()
        };
        assert_eq!(output, json!({ "runId": id }));
        let after = s.store.run(id).await.unwrap();
        for field in ["id", "taskId", "workspace", "sessionId", "snapshot"] {
            assert_eq!(after[field], before[field], "{surface}: {field}");
        }
        assert_eq!(after["status"], "queued");
        assert!(after["retry"].is_null());
        let checkpoint = s.store.checkpoint_boundary(id).await.unwrap().unwrap();
        assert_eq!(
            checkpoint["prepared"],
            json!({ "cwd": "/saved/workspace", "marker": "retained" })
        );
        assert_eq!(checkpoint["generation"], "retained-generation");
        assert_ne!(checkpoint["remainingMs"], 0);
        let repeated = call(&s, &configuration, "resume_run", json!({ "runId": id })).await;
        assert_eq!(repeated["result"]["isError"], true);
        s.worker.cancel(&s, id).await.unwrap();
        assert!(s.store.run(id).await.unwrap()["cancelRequestedAt"].is_number());
    }
    let missing = cairn_installation::config::id();
    let request = owner
        .authorize(common::request(
            "POST",
            &format!("/api/runs/{missing}/resume"),
        ))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = common::send(&app, request).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error: cairn_contracts::ResumeRunError =
        serde_json::from_value(common::read_json(response).await).unwrap();
    assert_eq!(error.error, "Run not found");
    for run_id in ["not-a-uuid".to_owned(), cairn_installation::config::id()] {
        let response = call(&s, &configuration, "resume_run", json!({ "runId": run_id })).await;
        assert_eq!(response["result"]["isError"], true);
    }
    let page = call(
        &s,
        &configuration,
        "search",
        json!({ "query": "resume_run", "detail": "schema" }),
    )
    .await;
    let operation = result(&page)["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "resume_run")
        .unwrap();
    assert_eq!(
        operation["inputSchema"]["properties"]["runId"]["format"],
        "uuid"
    );
    assert_eq!(
        operation["outputSchema"]["properties"]["runId"]["format"],
        "uuid"
    );
}

#[tokio::test]
async fn concurrent_resume_run_publishes_once_and_a_revoked_grant_cannot_resume() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let target = saved_mission(&s, &main).await;
    let id = text(&target, "id");
    let args = json!({ "runId": id });
    let (one, two) = tokio::join!(
        call(&s, &configuration, "resume_run", args.clone()),
        call(&s, &configuration, "resume_run", args),
    );
    let successes = [&one, &two]
        .iter()
        .filter(|response| response["result"]["isError"] != true)
        .count();
    assert_eq!(successes, 1, "{one} {two}");
    let run_id = id.to_owned();
    let events = s
        .store
        .read(move |db| db.events_fixture(&run_id, 0, 100))
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["text"] == "Resume requested")
            .count(),
        1
    );
    s.worker.cancel(&s, id).await.unwrap();
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let (status, _) = request(
        &s,
        token,
        "tools/call",
        common::mcp_arguments("resume_run", &json!({ "runId": id })),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
    assert!(s.store.run(id).await.unwrap()["cancelRequestedAt"].is_number());
}

#[tokio::test]
async fn beacon_account_methods_are_absent_from_both_tools() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let public = common::relay_fixture::grant(&s, &["read", "write", "run", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, bearer) in [("/mcp", public.as_str()), ("/mcp-installation", token)] {
        let (_, discovery) = node_management_call(
            &app,
            endpoint,
            bearer,
            "search",
            json!({ "names": ["list_account_methods"], "detail": "schema" }),
        )
        .await;
        let operations = result(&discovery)["operations"].as_array().unwrap();
        assert!(operations.is_empty(), "{discovery}");

        let (_, denied) =
            node_management_call(&app, endpoint, bearer, "list_account_methods", json!({})).await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
        assert!(denied.to_string().contains("Unknown operation"), "{denied}");
    }
}

#[tokio::test]
async fn typed_project_lifecycle_has_the_same_contract_on_both_tools() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let input = json!({
            "creationId": cairn_installation::config::id(),
            "settings": { "name": "Typed project", "path": root.path() },
        });
        let (_, created) =
            node_management_call(&app, endpoint, token, "save_project", input.clone()).await;
        let first = result(&created).clone();
        let project_id = first["id"].clone();
        let (_, read) = node_management_call(
            &app,
            endpoint,
            token,
            "get_project",
            json!({ "projectId": project_id }),
        )
        .await;
        assert_eq!(result(&read), &first);
        let replacement = json!({
            "projectId": project_id,
            "revision": first["revision"],
            "settings": { "name": "Replaced", "path": root.path(), "sourceMode": "local" },
        });
        let (_, replaced) = node_management_call(
            &app,
            endpoint,
            token,
            "replace_project",
            replacement.clone(),
        )
        .await;
        assert_eq!(result(&replaced)["name"], "Replaced");
        let (_, stale) =
            node_management_call(&app, endpoint, token, "replace_project", replacement).await;
        assert_eq!(stale["result"]["isError"], true, "{stale}");
        let (_, replay) = node_management_call(&app, endpoint, token, "save_project", input).await;
        assert_eq!(result(&replay), result(&replaced));
        let (_, listing) =
            node_management_call(&app, endpoint, token, "list_projects", json!({})).await;
        assert!(
            result(&listing)["projects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == result(&replaced))
        );
        let (_, denied) = node_management_call(
            &app,
            "/mcp",
            &read_token,
            "delete_project",
            json!({ "projectId": project_id }),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true);
        let (_, deleted) = node_management_call(
            &app,
            endpoint,
            token,
            "delete_project",
            json!({ "projectId": project_id }),
        )
        .await;
        assert_eq!(result(&deleted)["deleted"], true);
        let (_, missing) = node_management_call(
            &app,
            endpoint,
            token,
            "get_project",
            json!({ "projectId": project_id }),
        )
        .await;
        assert_eq!(missing["result"]["isError"], true);
    }
}

#[tokio::test]
async fn typed_agent_profiles_preview_and_avatars_match_on_both_tools() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let connection = s.mcp_fixture( json!({
        "name": "Selected tools", "transport": "http", "url": "http://127.0.0.1:9/mcp", "allowPrivateNetwork": true,
    }), None).await.unwrap();
    let connection_id = connection["id"].as_str().unwrap();
    for installation in [false, true] {
        let invoke = |name, args| {
            let configuration = &configuration;
            let s = &s;
            let token = &token;
            let app = &app;
            async move {
                if installation {
                    call(s, configuration, name, args).await
                } else {
                    let (status, response) =
                        node_management_call(app, "/mcp", token, name, args).await;
                    assert_eq!(status, StatusCode::OK);
                    response
                }
            }
        };
        let malformed: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/agent-contracts.json"))
                .unwrap();
        for command in malformed["invalidCreates"]
            .as_array()
            .unwrap()
            .iter()
            .skip(7)
        {
            let rejected = invoke("save_agent", command.clone()).await;
            assert_eq!(rejected["result"]["isError"], true, "{rejected}");
        }
        let command = create_profile(json!({ "name": "Typed profile" }));
        let created = invoke("save_agent", command).await;
        let created: cairn_contracts::Agent =
            serde_json::from_value(result(&created).clone()).unwrap();
        let settings = created.settings.clone();
        let replacing = cairn_contracts::ReplaceAgentInput {
            agent_id: created.id,
            revision: created.revision.clone(),
            settings: settings.clone(),
            review: cairn_contracts::AgentReview::default(),
        };
        // Creation starts avatar work independently. Settle it before observing preview writes.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let profile = s
                    .store
                    .agent(&created.id.to_string())
                    .await
                    .unwrap()
                    .unwrap();
                if profile
                    .avatar
                    .as_ref()
                    .is_none_or(|avatar| avatar.status != "generating")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let before = s.store.revision();
        let preview = invoke("preview_agent", serde_json::to_value(&replacing).unwrap()).await;
        let preview: cairn_contracts::AgentPreview =
            serde_json::from_value(result(&preview).clone()).unwrap();
        assert_eq!(preview.runs.queued + preview.runs.running, 0);
        assert!(preview.delegations.is_empty());
        assert_eq!(
            s.store.revision(),
            before,
            "Preview must not write or notify"
        );
        let replaced = invoke("replace_agent", serde_json::to_value(&replacing).unwrap()).await;
        let replaced: cairn_contracts::Agent =
            serde_json::from_value(result(&replaced).clone()).unwrap();
        assert_ne!(created.revision, replaced.revision);
        let stale = invoke("replace_agent", serde_json::to_value(&replacing).unwrap()).await;
        assert_eq!(stale["result"]["isError"], true);
        let mut expanded = replacing.clone();
        expanded.revision = replaced.revision.clone();
        expanded.settings.access.projects = None;
        let denied = invoke("replace_agent", serde_json::to_value(expanded).unwrap()).await;
        assert_eq!(
            denied["result"]["isError"], true,
            "An agent cannot expand permissions"
        );
        assert_eq!(
            s.store
                .agent(&created.id.to_string())
                .await
                .unwrap()
                .unwrap(),
            replaced
        );
        let configured = invoke("get_agent_avatar_configuration", json!({})).await;
        assert_eq!(result(&configured), &json!({ "configured": false }));
        let unavailable = invoke("generate_agent_avatar", json!({ "agentId": created.id })).await;
        assert_eq!(unavailable["result"]["isError"], true);
        let mut image = include_bytes!("../../../tests/fixtures/artifacts/thumbnail.png").to_vec();
        image.resize(200_000, 0); // A valid image beyond the ordinary small-command body limit.
        let uploaded = invoke(
            "set_agent_avatar",
            json!({ "agentId": created.id, "data": STANDARD.encode(image) }),
        )
        .await;
        let uploaded: cairn_contracts::Agent =
            serde_json::from_value(result(&uploaded).clone()).unwrap();
        assert_eq!(uploaded.avatar.as_ref().unwrap().status, "ready");
        assert_eq!(
            uploaded.revision, replaced.revision,
            "Portrait completion preserves the settings revision"
        );
        let read = invoke("get_agent", json!({ "agentId": created.id })).await;
        assert_eq!(result(&read), &serde_json::to_value(&uploaded).unwrap());
        // The owner grants a restricted connection. Agents may remove it, but cannot
        // turn its selected tools into unrestricted tools while retaining the connection.
        let granted = s
            .agent_fixture(
                json!({ "access": {
            "mcps": [connection_id], "mcpTools": { connection_id: ["read"] },
        } }),
                Some(&created.id.to_string()),
            )
            .await
            .unwrap();
        let granted: cairn_contracts::Agent = serde_json::from_value(granted).unwrap();
        let mut reduced = cairn_contracts::ReplaceAgentInput {
            agent_id: granted.id,
            revision: granted.revision.clone(),
            settings: granted.settings,
            review: cairn_contracts::AgentReview::default(),
        };
        reduced.settings.access.mcp_tools.clear();
        let expanded = invoke("replace_agent", serde_json::to_value(&reduced).unwrap()).await;
        assert_eq!(expanded["result"]["isError"], true, "{expanded}");
        assert_eq!(
            s.store
                .agent(&created.id.to_string())
                .await
                .unwrap()
                .unwrap()
                .revision,
            reduced.revision
        );
        reduced.settings.access.mcps = Some(Vec::new());
        let removed = invoke("replace_agent", serde_json::to_value(reduced).unwrap()).await;
        assert_eq!(result(&removed)["access"]["mcps"], json!([]));
        assert_eq!(result(&removed)["access"]["mcpTools"], json!({}));

        let deleted = invoke("delete_agent", json!({ "agentId": created.id })).await;
        assert_eq!(result(&deleted), &json!({ "deleted": true }));
        assert!(
            s.store
                .agent(&created.id.to_string())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            s.store
                .kv(&format!("agent-avatar:{}", created.id))
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn automatic_agent_priorities_round_trip_through_both_management_tools() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let nodes = [
        cairn_installation::config::id(),
        cairn_installation::config::id(),
    ];
    for id in &nodes {
        s.store
            .put(
                "nodes",
                json!({ "id": id, "name": id, "capabilities": { "backend": "firecracker" } }),
            )
            .await
            .unwrap();
    }
    let target = s.agent_fixture(json!({
        "name": "Automatic placement", "access": { "backend": "auto", "nodes": nodes }, "nodePriorities": nodes,
    }), None).await.unwrap();
    for internal in [false, true] {
        let mut profile = s.store.agent(text(&target, "id")).await.unwrap().unwrap();
        profile.settings.node_priorities.reverse();
        let input = serde_json::to_value(cairn_contracts::ReplaceAgentInput {
            agent_id: profile.id,
            revision: profile.revision,
            settings: profile.settings.clone(),
            review: cairn_contracts::AgentReview::default(),
        })
        .unwrap();
        let response = if internal {
            call(&s, &configuration, "replace_agent", input).await
        } else {
            public_call(&s, &["manage"], "replace_agent", input).await
        };
        assert_ne!(response["result"]["isError"], true, "{response}");
        let saved: cairn_contracts::Agent =
            serde_json::from_value(result(&response).clone()).unwrap();
        assert_eq!(saved.settings, profile.settings);
        let read = if internal {
            call(
                &s,
                &configuration,
                "get_agent",
                json!({ "agentId": saved.id }),
            )
            .await
        } else {
            public_call(&s, &["read"], "get_agent", json!({ "agentId": saved.id })).await
        };
        let read: cairn_contracts::Agent = serde_json::from_value(result(&read).clone()).unwrap();
        assert_eq!(read.settings.access.backend, "auto");
        assert_eq!(
            read.settings.node_priorities,
            saved.settings.node_priorities
        );
        assert_eq!(read.settings.access.nodes, Some(nodes.to_vec()));
    }
}

fn mission_settings(mission: &Value) -> Value {
    let mut settings = mission.clone();
    settings.as_object_mut().unwrap().retain(|key, _| {
        [
            "name",
            "prompt",
            "agentId",
            "projectId",
            "skills",
            "tags",
            "cron",
            "timezone",
            "enabled",
            "archived",
            "worktree",
        ]
        .contains(&key.as_str())
    });
    settings
}

#[tokio::test]
async fn typed_mcp_commands_share_revisions_secret_redaction_and_human_oauth_boundaries() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let public = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let read_only = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", public.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let input = json!({
            "name":"Typed MCP",
            "url":"https://example.test/mcp",
            "auth":"oauth",
            "clientSecret":"synthetic-secret",
            "enabled":false,
            "enabledTools":[],
        });
        let (_, created) =
            node_management_call(&app, endpoint, token, "create_mcp", input.clone()).await;
        let view = result(&created).clone();
        assert_eq!(view["hasClientSecret"], true, "{created}");
        assert_eq!(view["enabledTools"], json!([]));
        for key in [
            "clientSecret",
            "token",
            "env",
            "tokens",
            "verifier",
            "discovery",
        ] {
            assert!(view.get(key).is_none());
        }
        let id = view["id"].as_str().unwrap();
        let before = s.mcps.server_secrets(&s, id).await.unwrap();
        let (_, begin) = node_management_call(
            &app,
            endpoint,
            token,
            "begin_mcp_connection",
            json!({"id":id,"native":true}),
        )
        .await;
        assert!(
            result(&begin)["url"].as_str().unwrap().ends_with("/mcps"),
            "{begin}"
        );
        let (_, status) = node_management_call(
            &app,
            endpoint,
            token,
            "mcp_connection_status",
            json!({"id":id}),
        )
        .await;
        assert_eq!(result(&status)["pending"], false);
        assert_eq!(
            s.mcps.server_secrets(&s, id).await.unwrap(),
            before,
            "An agent cannot initiate a human session or mutate OAuth credentials"
        );
        let settings = serde_json::to_value(
            serde_json::from_value::<cairn_contracts::McpInput>(input).unwrap(),
        )
        .unwrap();
        let command = json!({"id":id,"revision":view["revision"],"connection":settings});
        for omitted in ["enabled", "enabledTools"] {
            let mut incomplete = command.clone();
            incomplete["connection"]
                .as_object_mut()
                .unwrap()
                .remove(omitted);
            let (_, denied) =
                node_management_call(&app, endpoint, token, "update_mcp", incomplete).await;
            assert_eq!(denied["result"]["isError"], true, "{denied}");
            let unchanged = s.mcps.server(&s, id).await.unwrap();
            assert_eq!(unchanged.revision, 1);
            assert!(!unchanged.enabled);
            assert_eq!(unchanged.enabled_tools, Some(vec![]));
        }
        let (_, saved) =
            node_management_call(&app, endpoint, token, "update_mcp", command.clone()).await;
        assert_eq!(result(&saved)["revision"], 2);
        let (_, stale) = node_management_call(&app, endpoint, token, "update_mcp", command).await;
        assert_eq!(stale["result"]["isError"], true, "{stale}");
        assert!(stale.to_string().contains("changed elsewhere"));
        let (_, cancel) = node_management_call(
            &app,
            endpoint,
            token,
            "cancel_mcp_connection",
            json!({"id":id}),
        )
        .await;
        assert_eq!(result(&cancel)["completed"], true);
        assert_eq!(s.mcps.server_secrets(&s, id).await.unwrap(), before);
        let (_, deleted) =
            node_management_call(&app, endpoint, token, "delete_mcp", json!({"id":id})).await;
        assert_eq!(result(&deleted)["completed"], true);
        assert!(s.store.mcp_server(id).await.unwrap().is_none());
    }
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read_only,
        "create_mcp",
        json!({"name":"denied","url":"https://example.test"}),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(s.mcps.list(&s).await.unwrap().is_empty());
}

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
        let mut account = s
            .accounts
            .create(
                &s,
                cairn_installation::provider::Provider::Claude,
                "Alternative",
            )
            .await
            .unwrap();
        account["state"] = "ready".into();
        s.store.put("agentAccounts", account).await.unwrap();
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
