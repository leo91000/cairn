mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::hex_digest, config::MAIN_AGENT_ID, mcps::channel, run_status::RunStatus,
    service::Service, validation::parse,
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

async fn enabled_main(s: &Arc<Service>) -> Value {
    s.agent(
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

async fn call(s: &Arc<Service>, configuration: &Value, name: &str, args: Value) -> Value {
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (status, result) = request(
        s,
        token,
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

fn result(response: &Value) -> &Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    &response["result"]["structuredContent"]["result"]
}

#[tokio::test]
async fn installation_access_is_opt_in_and_older_profile_updates_preserve_it() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = s.get("agents", MAIN_AGENT_ID).await.unwrap();
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
    assert_eq!(
        configuration["claudeMcps"]["mcpServers"]["cairn_installation"]["url"],
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
        .agent(
            json!({ "name": "Older editor", "access": { "nodes": null } }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
    assert_eq!(saved["access"]["installation"], true);
    let update = parse(
        "mcp:update_agent",
        json!({ "id": MAIN_AGENT_ID, "agent": { "access": {} } }),
    )
    .unwrap();
    assert!(update["agent"]["access"].get("installation").is_none());
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
    let tools = catalog["result"]["tools"].as_array().unwrap();
    for name in ["create_task", "list_tasks", "get_run", "run_task"] {
        assert!(tools.iter().any(|tool| tool["name"] == name));
    }
    assert!(
        !tools
            .iter()
            .any(|tool| tool["name"] == "save_agent" || tool["name"] == "create_mcp")
    );
    assert!(tools.iter().all(|tool| tool.get("_meta").is_none()));
    assert_eq!(
        call(
            &s,
            &configuration,
            "save_agent",
            json!({ "name": "Escalate" })
        )
        .await["result"]["isError"],
        true
    );

    let created = call(
        &s,
        &configuration,
        "create_task",
        json!({
            "name": "Weekly audit", "prompt": "Audit", "agentId": MAIN_AGENT_ID,
            "cron": "0 9 * * 1", "timezone": "UTC", "worktree": false,
        }),
    )
    .await;
    let task = result(&created);
    assert!(task["nextRun"].is_number());
    assert_eq!(task["authorId"], "fixture-owner");
    let tasks = call(&s, &configuration, "list_tasks", json!({})).await;
    assert!(
        result(&tasks)
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
        "update_task",
        json!({ "id": task["id"], "task": update }),
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
    let history = call(&s, &configuration, "get_run", json!({ "runId": run["id"] })).await;
    assert_eq!(result(&history)["run"]["id"], run["id"]);
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
async fn scoped_agents_cannot_read_or_launch_more_privileged_work() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = s.get("agents", MAIN_AGENT_ID).await.unwrap();
    let (foreign, _) = active_run(&s, &main).await;
    let scoped = s
        .agent(
            json!({ "name": "Restricted", "access": {
                "installation": true, "projects": [], "skills": [], "mcps": [],
                "github": false, "sandbox": "read-only",
            }}),
            None,
        )
        .await
        .unwrap();
    let (own, configuration) = active_run(&s, &scoped).await;
    let (_, catalog) = request(
        &s,
        configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
            .as_str()
            .unwrap(),
        "tools/list",
        json!({}),
    )
    .await;
    let create = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "create_task")
        .unwrap();
    assert_eq!(
        create["inputSchema"]["properties"]["agentId"]["default"],
        scoped["id"]
    );
    assert_eq!(
        create["inputSchema"]["properties"]["agentId"]["type"],
        "string"
    );
    let peer = s
        .agent(
            json!({ "name": "Same policy, different credentials", "access": scoped["access"] }),
            None,
        )
        .await
        .unwrap();
    let (peer_run, _) = active_run(&s, &peer).await;
    let created = call(
        &s,
        &configuration,
        "create_task",
        json!({
            "name": "Follow-up", "prompt": "Continue reviewing", "worktree": false,
        }),
    )
    .await;
    assert_eq!(result(&created)["agentId"], scoped["id"]);
    for name in ["get_run", "read_run_content", "cancel_run"] {
        assert_eq!(
            call(&s, &configuration, name, json!({ "runId": peer_run["id"] })).await["result"]["isError"],
            true,
            "An identical access policy must not delegate another agent's credentials: {name}"
        );
    }
    assert_eq!(
        call(
            &s,
            &configuration,
            "run_task",
            json!({ "taskId": peer_run["taskId"] })
        )
        .await["result"]["isError"],
        true
    );
    assert_eq!(
        call(
            &s,
            &configuration,
            "create_task",
            json!({
                "name": "Borrow credentials", "prompt": "Inspect", "agentId": peer["id"],
            })
        )
        .await["result"]["isError"],
        true
    );
    for name in ["get_run", "read_run_content", "cancel_run"] {
        assert_eq!(
            call(&s, &configuration, name, json!({ "runId": foreign["id"] })).await["result"]["isError"],
            true,
            "{name}"
        );
    }
    assert_eq!(
        call(
            &s,
            &configuration,
            "run_task",
            json!({ "taskId": foreign["taskId"] })
        )
        .await["result"]["isError"],
        true
    );
    let tasks_before = s.store.list("tasks").await.unwrap().len();
    assert_eq!(
        call(
            &s,
            &configuration,
            "create_task",
            json!({ "name": "Escape", "prompt": "Escape", "agentId": MAIN_AGENT_ID })
        )
        .await["result"]["isError"],
        true
    );
    assert_eq!(s.store.list("tasks").await.unwrap().len(), tasks_before);
    let agents = call(&s, &configuration, "list_agents", json!({})).await;
    assert_eq!(result(&agents).as_array().unwrap().len(), 1);
    let tasks = call(&s, &configuration, "list_tasks", json!({})).await;
    assert_eq!(result(&tasks).as_array().unwrap().len(), 2);
    let runs = call(&s, &configuration, "list_runs", json!({ "limit": 1 })).await;
    assert_eq!(result(&runs)[0]["id"], own["id"]);
    let next = call(
        &s,
        &configuration,
        "list_runs",
        json!({ "limit": 1, "offset": 1 }),
    )
    .await;
    assert!(result(&next).as_array().unwrap().is_empty());
    let own_history = call(&s, &configuration, "get_run", json!({ "runId": own["id"] })).await;
    assert_eq!(result(&own_history)["run"]["id"], own["id"]);
    let own_task = s
        .get("tasks", own["taskId"].as_str().unwrap())
        .await
        .unwrap();
    let mut widened = own_task.clone();
    widened["agentId"] = MAIN_AGENT_ID.into();
    assert_eq!(
        call(
            &s,
            &configuration,
            "update_task",
            json!({ "id": own_task["id"], "task": widened })
        )
        .await["result"]["isError"],
        true
    );
    assert_eq!(
        s.get("tasks", own_task["id"].as_str().unwrap())
            .await
            .unwrap()["agentId"],
        scoped["id"]
    );
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
        .patch_run(id, json!({ "chatExecution": { "messageId": "new-turn" } }))
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .patch_run(id, json!({ "chatExecution": null }))
        .await
        .unwrap();
    s.agent(
        json!({ "name": "Main", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    enabled_main(&s).await;
    s.store
        .patch_run(id, json!({ "status": RunStatus::Succeeded }))
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .patch_run(
            id,
            json!({ "status": RunStatus::Running, "cancelRequestedAt": 1 }),
        )
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .patch_run(id, json!({ "cancelRequestedAt": null }))
        .await
        .unwrap();
    s.mcps.revoke_run(&s, id).await.unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn legacy_grants_do_not_acquire_installation_access_and_vm_channels_bind_the_run() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let key = format!("mcp-grant:{}", hex_digest(token));
    let mut legacy = s.store.kv(&key).await.unwrap().unwrap();
    legacy.as_object_mut().unwrap().remove("installation");
    s.store.set(&key, legacy, None).await.unwrap();
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
