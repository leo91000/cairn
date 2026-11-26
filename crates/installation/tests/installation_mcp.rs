use crate::common::run_fixture::DbRunFixture;
use cairn_contracts::EventKind;
use common::account_fixture::AccountFixtures;
use common::node_fixture::*;
#[allow(unused_imports)]
use common::{PluginServiceFixture as _, PluginStoreFixture as _};

use common::AgentFixture;
use common::McpFixture;
use common::MissionFixture;
use common::ProjectFixture;
use common::run_fixture::RunFixture;

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
    finish_resource_call(s, "/mcp-installation", token, name, result).await
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
    let response = common::read_json(response).await;
    finish_resource_call(s, "/mcp", &token, name, response).await
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

async fn account_http(
    app: &axum::Router,
    owner: &common::RelayContext,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = owner
        .authorize(common::request(method, path))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(app, request).await;
    (response.status(), common::read_json(response).await)
}

async fn finish_resource_call(
    s: &Arc<Service>,
    endpoint: &str,
    token: &str,
    name: &str,
    mut response: Value,
) -> Value {
    if !matches!(
        name,
        "import_project"
            | "list_source_projects"
            | "import_skill"
            | "get_skill_source_status"
            | "update_skill_source"
    ) || response["result"]["isError"] == true
    {
        return response;
    }
    let id = result(&response)["operation"]
        .as_str()
        .expect("Resource preparation id")
        .to_owned();
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let (_, polled) = node_management_call(
                &app,
                endpoint,
                token,
                "get_plugin_operation",
                json!({"operationId": id}),
            )
            .await;
            if polled["result"]["isError"] == true {
                return polled;
            }
            let state = result(&polled);
            match text(state, "state") {
                "completed" => {
                    let value = common::resource_output(&state["result"]);
                    response["result"]["structuredContent"]["result"] = value.clone();
                    response["result"]["content"][0]["text"] = value.to_string().into();
                    return response;
                }
                "failed" | "cancelled" => {
                    response["result"]["isError"] = true.into();
                    response["result"]["content"][0]["text"] = state["error"].clone();
                    return response;
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("Resource tool preparation did not finish")
}

async fn finish_plugin_resource_call(
    s: &Arc<Service>,
    endpoint: &str,
    token: &str,
    mut response: Value,
) -> Value {
    if response["result"]["isError"] == true {
        return response;
    }
    let id = result(&response)["operation"]
        .as_str()
        .expect("Resource read operation")
        .to_owned();
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let (status, polled) = node_management_call(
                &app,
                endpoint,
                token,
                "get_plugin_resource_operation",
                json!({"operationId":id}),
            )
            .await;
            if !status.is_success()
                || polled["error"].is_object()
                || polled["result"]["isError"] == true
            {
                response["result"]["isError"] = true.into();
                response["result"]["content"][0]["text"] = polled.to_string().into();
                return response;
            }
            let state = result(&polled);
            match text(state, "state") {
                "completed" => {
                    let value = resource_read_output(&state["result"]);
                    response["result"]["structuredContent"]["result"] = value.clone();
                    response["result"]["content"][0]["text"] = value.to_string().into();
                    return response;
                }
                "failed" | "cancelled" => {
                    response["result"]["isError"] = true.into();
                    response["result"]["content"][0]["text"] = state["error"].clone();
                    return response;
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .expect("Resource read operation did not finish")
}

fn resource_read_output(value: &Value) -> Value {
    match text(value, "kind") {
        "resourcePage" => value["page"].clone(),
        "resourceAncestry" => value["resolution"].clone(),
        other => panic!("Unexpected resource read result {other}"),
    }
}

async fn poll_resource_http(app: &axum::Router, id: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let response = common::send(
                app,
                common::request("GET", &format!("/api/plugins/resources/operations/{id}"))
                    .extension(cairn_installation::auth::InstallationIdentity::trusted(
                        cairn_installation::auth::InstallationRole::Owner,
                        "fixture-owner",
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let state = common::read_json(response).await;
            match text(&state, "state") {
                "completed" => return resource_read_output(&state["result"]),
                "failed" | "cancelled" => panic!("{state}"),
                _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("HTTP resource operation did not finish")
}

#[path = "installation_mcp/accounts_and_plugins.rs"]
mod accounts_and_plugins;
#[path = "installation_mcp/chats_and_plugins.rs"]
mod chats_and_plugins;
#[path = "installation_mcp/code_mode_and_missions.rs"]
mod code_mode_and_missions;
#[path = "installation_mcp/nodes_and_agents.rs"]
mod nodes_and_agents;
#[path = "installation_mcp/skills_and_resources.rs"]
mod skills_and_resources;
