use crate::common;
use crate::common::AgentFixture;
use axum::{body::Body, http::StatusCode};
use cairn_installation::{config::MAIN_AGENT_ID, run_status::RunStatus, service::Service};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

pub(crate) async fn service(root: &TempDir) -> Arc<Service> {
    std::fs::create_dir(root.path().join("home")).unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s
}

pub(crate) async fn active_run(s: &Service, agent: &Value) -> (Value, Value) {
    let task = s
        .task(
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
    let run = s
        .store
        .patch_run(
            run["id"].as_str().unwrap(),
            json!({ "status": RunStatus::Running }),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(s, &run).await.unwrap();
    (run, configuration)
}

pub(crate) async fn enabled_main(s: &Arc<Service>) -> Value {
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap()
}

pub(crate) async fn request(
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

pub(crate) fn create_profile(mut settings: Value) -> Value {
    let mut access = json!({ "backend": "firecracker", "nodes": [], "projects": [], "skills": [], "mcps": [], "mcpTools": {}, "installation": false, "delegation": false, "sandbox": "read-only" });
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

pub(crate) async fn replacement(s: &Service, args: Value) -> Value {
    let current = s
        .store
        .agent(args["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut settings = serde_json::to_value(&current.settings).unwrap();
    let mut patch = args["agent"].clone();
    let review = json!({ "nativeNodeReviews": patch.as_object_mut().unwrap().remove("nativeNodeReviews").unwrap_or_else(|| json!([])), "reapproveDelegations": patch.as_object_mut().unwrap().remove("reapproveDelegations").unwrap_or_else(|| json!([])) });
    if let Some(access) = patch.get_mut("access") {
        let mut complete = settings["access"].clone();
        cairn_installation::store::merge(&mut complete, access);
        *access = complete;
    }
    cairn_installation::store::merge(&mut settings, &patch);
    json!({ "agentId": current.id, "revision": current.revision, "settings": settings, "review": review })
}

pub(crate) async fn call(
    s: &Arc<Service>,
    configuration: &Value,
    name: &str,
    args: Value,
) -> Value {
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

pub(crate) fn result(response: &Value) -> &Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    &response["result"]["structuredContent"]["result"]
}

pub(crate) async fn public_call(
    s: &Arc<Service>,
    scopes: &[&str],
    name: &str,
    args: Value,
) -> Value {
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
