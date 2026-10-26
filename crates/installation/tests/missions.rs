mod common;

use common::AgentFixture;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::{MAIN_AGENT_ID, id},
    service::Service,
};
use serde_json::{Value, json};
use std::sync::Arc;

async fn request(s: &Arc<Service>, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let request = common::request(method, path)
        .extension(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(&app, request).await;
    (response.status(), common::read_json(response).await)
}

#[tokio::test]
async fn mission_commands_preserve_revisions_retries_relations_and_frozen_runs() {
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    std::fs::create_dir_all(&s.config.home).unwrap();
    let creation = id();
    let settings = json!({"name":"  Mission  ","prompt":"  Inspect  ","agentId":MAIN_AGENT_ID,"cron":"0 9 * * *","skills":[],"tags":["review","review"]});
    let command = json!({"creationId":creation,"settings":settings});
    let (status, first) = request(&s, "POST", "/api/tasks", command.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["name"], "Mission");
    assert_eq!(first["prompt"], "Inspect");
    assert_eq!(first["authorId"], "fixture-owner");
    assert_eq!(first["authorAccessId"], "fixture-owner");
    assert_eq!(first["skills"], json!([]));
    assert_eq!(first["tags"], json!(["review", "review"]));
    let path = format!("/api/tasks/{creation}");
    let replacement = json!({"taskId":creation,"revision":first["revision"],"settings":{ "name":"Updated","prompt":"New work","agentId":MAIN_AGENT_ID,"skills":null }});
    let (left, right) = tokio::join!(
        request(&s, "PUT", &path, replacement.clone()),
        request(&s, "PUT", &path, replacement)
    );
    assert!(matches!(
        (left.0, right.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
    ));
    let saved = if left.0 == StatusCode::OK {
        left.1
    } else {
        right.1
    };
    assert_eq!(saved["authorId"], first["authorId"]);
    assert_eq!(saved["authorAccessId"], first["authorAccessId"]);
    assert_eq!(saved["createdAt"], first["createdAt"]);
    assert_eq!(saved["skills"], Value::Null);
    assert_ne!(saved["revision"], first["revision"]);
    assert_eq!(request(&s, "POST", "/api/tasks", command).await.1, saved);
    let replayed = Service::new(s.config.clone()).await.unwrap();
    assert_eq!(
        serde_json::to_value(replayed.store.mission(&creation).await.unwrap().unwrap()).unwrap(),
        saved
    );
    assert_eq!(request(&s, "GET", &path, Value::Null).await.1, saved);
    assert_eq!(
        request(&s, "GET", "/api/tasks", Value::Null).await.1["tasks"],
        json!([saved])
    );

    let run = s.enqueue(&creation, "manual", None).await.unwrap();
    let (status, edited) = request(
        &s,
        "PUT",
        &path,
        json!({"taskId":creation,"revision":saved["revision"],"settings":settings}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(
        s.store.run(run["id"].as_str().unwrap()).await.unwrap()["snapshot"]["task"],
        saved
    );
    assert_eq!(
        request(&s, "DELETE", &path, Value::Null).await.0,
        StatusCode::CONFLICT
    );
    s.store
        .patch_run(run["id"].as_str().unwrap(), json!({"status":"failed"}))
        .await
        .unwrap();
    assert_eq!(
        request(&s, "DELETE", &path, Value::Null).await.0,
        StatusCode::OK
    );
    assert!(s.store.mission(&creation).await.unwrap().is_none());
    assert_eq!(
        s.store.run(run["id"].as_str().unwrap()).await.unwrap()["snapshot"]["task"],
        saved
    );
    s.store
        .read(|db| {
            let count: i64 = db.0.query_row(
                "SELECT (SELECT count(*) FROM mission_tags)+(SELECT count(*) FROM mission_skills)",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(count, 0);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn mission_commands_reject_forged_authorship_and_sql_rejects_author_changes() {
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    let settings = json!({"name":"Mission","prompt":"Work","agentId":MAIN_AGENT_ID});
    let (_, mission) = request(&s, "POST", "/api/tasks", json!({"settings":settings})).await;
    let mut forged = settings.clone();
    forged["authorId"] = "another-account".into();
    assert_eq!(
        request(&s, "POST", "/api/tasks", json!({"settings":forged}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut stored = s
        .store
        .mission(mission["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    stored.author_access_id = Some("new-membership".into());
    assert!(
        s.store
            .transaction(move |db| db.save_mission(&stored))
            .await
            .is_err()
    );
    assert_eq!(
        request(
            &s,
            "GET",
            &format!("/api/tasks/{}", mission["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1,
        mission
    );
    assert!(s.store.put("tasks", mission.clone()).await.is_err());
    let path = format!("/api/tasks/{}", mission["id"].as_str().unwrap());
    assert_eq!(
        request(
            &s,
            "PUT",
            &path,
            json!({"taskId":mission["id"],"settings":settings})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut missing = settings;
    missing["agentId"] = id().into();
    assert_ne!(
        request(&s, "POST", "/api/tasks", json!({"settings":missing}))
            .await
            .0,
        StatusCode::OK
    );
}

async fn tool(app: &axum::Router, endpoint: &str, bearer: &str, name: &str, input: Value) -> Value {
    let request = common::request("POST", endpoint)
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":common::mcp_arguments(name,&input),
            })
            .to_string(),
        ))
        .unwrap();
    common::read_json(common::send(app, request).await).await
}

async fn installation_token(s: &Arc<Service>) -> String {
    use common::{AgentFixture, MissionFixture};
    let agent = s
        .agent_fixture(
            json!({"name":"Manager","access":{"installation":true}}),
            None,
        )
        .await
        .unwrap();
    let task = s
        .task_fixture(
            json!({"name":"Caller","prompt":"Manage","agentId":agent["id"]}),
            None,
        )
        .await
        .unwrap();
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    s.store
        .patch_run(run["id"].as_str().unwrap(), json!({"status":"running"}))
        .await
        .unwrap();
    let run = s.store.run(run["id"].as_str().unwrap()).await.unwrap();
    let config = s.mcps.run_configuration(s, &run).await.unwrap();
    config["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn result(response: &Value) -> &Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    assert!(response["error"].is_null(), "{response}");
    &response["result"]["structuredContent"]["result"]
}

#[tokio::test]
async fn mission_management_preview_and_activity_match_both_mcp_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    std::fs::create_dir_all(&s.config.home).unwrap();
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let token = installation_token(&s).await;
    let public = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    for (endpoint, token) in [("/mcp", &public), ("/mcp-installation", &token)] {
        let created=tool(&app,endpoint,token,"create_task",json!({"settings":{
            "name":"Scheduled work","prompt":"Check","agentId":MAIN_AGENT_ID,"cron":"0 9 * * *","timezone":"UTC",
        }})).await;
        let mission = result(&created);
        let get = tool(
            &app,
            endpoint,
            token,
            "get_task",
            json!({"taskId":mission["id"]}),
        )
        .await;
        assert_eq!(result(&get), mission);
        let listed = tool(&app, endpoint, token, "list_tasks", json!({})).await;
        assert!(
            result(&listed)["tasks"]
                .as_array()
                .unwrap()
                .contains(mission)
        );
        let paused=tool(&app,endpoint,token,"set_task_state",json!({"taskId":mission["id"],"revision":mission["revision"],"enabled":false,"archived":true})).await;
        assert_eq!(result(&paused)["nextRun"], Value::Null);
        assert_eq!(result(&paused)["authorId"], mission["authorId"]);
        let preview = tool(
            &app,
            endpoint,
            token,
            "preview_schedule",
            json!({"cron":"0 9 * * *","timezone":"UTC"}),
        )
        .await;
        assert_eq!(result(&preview)["occurrences"].as_array().unwrap().len(), 3);
        let activity = tool(&app, endpoint, token, "list_task_activity", json!({})).await;
        assert_eq!(result(&activity).as_array().unwrap().len(), 1);
        let deleted = tool(
            &app,
            endpoint,
            token,
            "delete_task",
            json!({"taskId":mission["id"]}),
        )
        .await;
        assert_eq!(result(&deleted)["deleted"], true);
    }
    let readonly = common::relay_fixture::grant(&s, &["read"]).await;
    let count = s.store.missions().await.unwrap().len();
    let denied = tool(
        &app,
        "/mcp",
        &readonly,
        "create_task",
        json!({"settings":{
            "name":"Denied","prompt":"Check","agentId":MAIN_AGENT_ID,
        }}),
    )
    .await;
    assert!(denied["result"]["isError"] == true || !denied["error"].is_null());
    assert_eq!(s.store.missions().await.unwrap().len(), count);
}

#[tokio::test]
async fn a_mission_prepared_before_installation_grant_revocation_cannot_commit() {
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    std::fs::create_dir_all(&s.config.home).unwrap();
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let token = installation_token(&s).await;
    let count = s.store.missions().await.unwrap().len();
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let routes = axum::Router::new().route(
        "/api/relay/fixture/task-authors",
        axum::routing::get({
            let entered = entered.clone();
            let release = release.clone();
            move || {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    axum::Json(cairn_protocol::TaskAuthorPolicy {
                        owner: cairn_protocol::TaskAuthorGrant {
                            account_id: "fixture-owner".into(),
                            access_id: "fixture-owner".into(),
                        },
                        members: Vec::new(),
                    })
                }
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    tokio::fs::write(
        s.config.data_dir.join("installation-relay/identity.json"),
        json!({
            "origin":origin,"installationId":"fixture","token":"fixture-only",
        })
        .to_string(),
    )
    .await
    .unwrap();
    let saving = tokio::spawn({
        let token = token.clone();
        async move {
            tool(
                &app,
                "/mcp-installation",
                &token,
                "create_task",
                json!({"settings":{
                    "name":"Revoked write","prompt":"Must not be saved","agentId":MAIN_AGENT_ID,
                }}),
            )
            .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let manager = s
        .store
        .agents()
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.settings.name == "Manager")
        .unwrap();
    s.agent_fixture(
        json!({"access":{"installation":false}}),
        Some(&manager.id.to_string()),
    )
    .await
    .unwrap();
    release.add_permits(1);
    let response = saving.await.unwrap();
    assert!(
        response["result"]["isError"] == true || !response["error"].is_null(),
        "{response}"
    );
    assert_eq!(s.store.missions().await.unwrap().len(), count);
    server.abort();
}
