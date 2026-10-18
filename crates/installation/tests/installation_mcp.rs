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
    let (status, result) =
        request(s, token, "tools/call", common::mcp_arguments(name, &args)).await;
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
async fn delete_agent_removes_an_unused_profile_and_preserves_completed_history() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent = s
        .agent(json!({ "name": "Retired maintainer" }), None)
        .await
        .unwrap();
    let id = agent["id"].as_str().unwrap();
    let (run, _) = active_run(&s, &agent).await;
    s.store
        .patch_run(
            run["id"].as_str().unwrap(),
            json!({ "status": RunStatus::Succeeded }),
        )
        .await
        .unwrap();
    s.remove("tasks", run["taskId"].as_str().unwrap())
        .await
        .unwrap();
    s.store
        .transaction({
            let id = id.to_owned();
            move |db| {
                db.set(
                    &format!("agent-github:{id}"),
                    &json!({ "token": "fixture-only" }),
                    None,
                )?;
                db.set(
                    &format!("agent-avatar:{id}"),
                    &json!({ "status": "ready" }),
                    None,
                )?;
                // A plugin instance (such as a 1Password service account) granting it.
                db.put(
                    "plugin-connections",
                    &json!({
                        "id": "fixture-account",
                        "pluginId": "onepassword",
                        "agentIds": [id, MAIN_AGENT_ID],
                    }),
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();

    let deleted = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(result(&deleted)["deleted"], true);
    assert!(s.store.get("agents", id).await.unwrap().is_none());
    for key in ["agent-github", "agent-avatar"] {
        assert!(s.store.kv(&format!("{key}:{id}")).await.unwrap().is_none());
    }
    assert_eq!(
        s.store
            .get("plugin-connections", "fixture-account")
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
        .agent(json!({ "name": "Assigned maintainer" }), None)
        .await
        .unwrap();
    let id = agent["id"].as_str().unwrap();
    let task = s
        .task(
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
    s.remove("tasks", task["id"].as_str().unwrap())
        .await
        .unwrap();

    let (run, _) = active_run(&s, &agent).await;
    let task_id = run["taskId"].as_str().unwrap();
    let mut task = s.get("tasks", task_id).await.unwrap();
    task["agentId"] = MAIN_AGENT_ID.into();
    s.task(task, Some(task_id)).await.unwrap();
    let busy = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(busy["result"]["isError"], true);
    assert_eq!(
        busy["result"]["content"][0]["text"],
        "This item has active work. Cancel or wait for the run first."
    );
    assert!(s.store.get("agents", id).await.unwrap().is_some());
}

#[tokio::test]
async fn delete_agent_cannot_use_a_revoked_installation_grant() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let agent = s
        .agent(json!({ "name": "Keep this profile" }), None)
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
            "arguments": { "id": agent["id"] },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        s.store
            .get("agents", agent["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_some()
    );
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
    assert_eq!(unknown["result"]["content"][0]["text"], "Unknown operation");
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

async fn public_call(s: &Arc<Service>, scopes: &[&str], name: &str, args: Value) -> Value {
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
        .agent(
            json!({
                "name": "Delegated manager", "access": { "installation": true, "projects": [], "github": false },
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
        .patch_run(
            run["id"].as_str().unwrap(),
            json!({ "cancelRequestedAt": 1 }),
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
        .patch_run(run_id, json!({ "status": RunStatus::Failed }))
        .await
        .unwrap();
    s.store
        .set(
            &format!("run-checkpoint:{run_id}"),
            json!({ "launched": true }),
            None,
        )
        .await
        .unwrap();
    let mut chat = s.chat_create(json!({})).await.unwrap();
    chat["runId"] = target["id"].clone();
    chat["restoredAt"] = 1.into();
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
            await cairn.update_agent(input);
            return await cairn.list_chats({});
        "#,
            "input": { "id": MAIN_AGENT_ID, "agent": { "access": access } },
        }),
    )
    .await;
    assert_eq!(revoked["result"]["isError"], true);
    let agents = public_call(&s, &["read"], "list_agents", json!({})).await;
    let main = result(&agents)
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
