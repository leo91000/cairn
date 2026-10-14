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
    assert!(configuration.get("claudeMcps").is_none());
    let claude = s
        .agent(
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
    let public: Value = serde_json::from_str(include_str!("../schemas/mcp-tools.json")).unwrap();
    let public_names: std::collections::BTreeSet<_> = public
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let installed_names: std::collections::BTreeSet<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(installed_names, public_names);
    assert!(tools.iter().all(|tool| tool.get("_meta").is_none()));

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
async fn installation_access_manages_resources_and_other_agents_work() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = s.get("agents", MAIN_AGENT_ID).await.unwrap();
    let (foreign, _) = active_run(&s, &main).await;
    let manager = s
        .agent(
            json!({
                "name": "Manager",
                "access": {
                    "installation": true,
                    "projects": [],
                    "skills": [],
                    "mcps": [],
                    "github": false,
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
        "update_agent",
        json!({
            "id": agent["id"],
            "agent": { "description": "Updated through MCP" }
        }),
    )
    .await;
    assert_eq!(result(&updated)["description"], "Updated through MCP");
    let agents = call(&s, &configuration, "list_agents", json!({})).await;
    assert!(
        result(&agents)
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
            "name": "Managed project",
            "path": project_path,
            "sourceMode": "local"
        }),
    )
    .await;
    let project_id = result(&project)["id"].clone();
    let projects = call(&s, &configuration, "list_projects", json!({})).await;
    assert!(
        result(&projects)
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
            "connection": {
                "name": "Renamed connection",
                "url": "https://example.com/mcp",
                "enabled": false
            }
        }),
    )
    .await;
    assert_eq!(result(&updated)["name"], "Renamed connection");
    let connections = call(&s, &configuration, "list_mcps", json!({})).await;
    assert!(
        result(&connections)
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
    assert_eq!(result(&disconnected)["disconnected"], true);
    let deleted = call(
        &s,
        &configuration,
        "delete_mcp",
        json!({ "id": connection_id }),
    )
    .await;
    assert_eq!(result(&deleted)["deleted"], true);
    let connections = call(&s, &configuration, "list_mcps", json!({})).await;
    assert!(result(&connections).as_array().unwrap().is_empty());

    let created = call(
        &s,
        &configuration,
        "create_task",
        json!({
            "name": "Delegate",
            "prompt": "Inspect",
            "agentId": MAIN_AGENT_ID,
            "worktree": false
        }),
    )
    .await;
    assert_eq!(result(&created)["agentId"], MAIN_AGENT_ID);
    let own_default = call(
        &s,
        &configuration,
        "create_task",
        json!({
            "name": "Continue",
            "prompt": "Continue",
            "worktree": false
        }),
    )
    .await;
    assert_eq!(result(&own_default)["agentId"], manager["id"]);
    let tasks = call(&s, &configuration, "list_tasks", json!({})).await;
    assert!(
        result(&tasks)
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
        "update_task",
        json!({
            "id": task["id"],
            "task": task
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
        .agent(
            json!({
                "name": "Skill author",
                "access": {
                    "installation": true,
                    "projects": [],
                    "skills": [],
                    "github": false
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
        .project(
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
    assert_eq!(unknown["result"]["content"][0]["text"], "Unknown tool");
}

#[tokio::test]
async fn management_discovery_cannot_publish_after_its_grant_is_revoked() {
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
        "/mcp",
        post({
            let started = started.clone();
            let release = release.clone();
            move |Json(request): Json<Value>| {
                let started = started.clone();
                let release = release.clone();
                async move {
                    let result = if request["method"] == "server/discover" {
                        json!({
                            "supportedVersions": ["2026-07-28"],
                            "capabilities": { "tools": {} }
                        })
                    } else {
                        assert_eq!(request["method"], "tools/list");
                        started.notify_one();
                        release.notified().await;
                        json!({ "tools": [] })
                    };
                    Json(json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": result
                    }))
                }
            }
        }),
    );
    let (origin, server) = common::serve_locally(app).await;
    let connection = s
        .mcps
        .save(
            &s,
            json!({
                "name": "Delayed discovery",
                "url": format!("{origin}/mcp"),
                "allowPrivateNetwork": true
            }),
            None,
        )
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
    assert_eq!(s.mcps.list(&s).await.unwrap(), before);
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
        .mcps
        .save(
            &s,
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
    s.vault.set(&id, &credentials).await.unwrap();
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
    assert!(s.vault.get(&id).await.unwrap().as_ref() == Some(&credentials));
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
