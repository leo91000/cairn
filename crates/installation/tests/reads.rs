use crate::common::run_fixture::DbRunFixture;
use crate::common::run_fixture::RunFixture;
use common::MissionFixture;

mod common;

use common::{RelayContext, relay_fixture::router};

use axum::{Router, body::Body, http::StatusCode};
use cairn_installation::{
    config::{MAIN_AGENT_ID, id, now},
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;

struct App {
    _root: TempDir,
    router: Router,
    service: Arc<Service>,
    owner: RelayContext,
    member: RelayContext,
}

impl App {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        let service = Service::new(common::config(root.path())).await.unwrap();
        let router = router(service.clone()).await.unwrap();
        let owner = RelayContext::new(&common::relay_fixture::context(&service).await);
        let member = RelayContext::new(
            &common::relay_fixture::member_context(&service, "fixture-member").await,
        );
        Self {
            _root: root,
            router,
            service,
            owner,
            member,
        }
    }

    async fn request(
        &self,
        who: &RelayContext,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let request = who
            .authorize(common::request(method, path))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&self.router, request).await;
        (response.status(), common::read_json(response).await)
    }

    async fn get(&self, who: &RelayContext, path: &str) -> Value {
        let (status, body) = self.request(who, "GET", path, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// How the account set the mission run aside, as the Fil lists it.
    async fn dismissal(&self, who: &RelayContext, run: &str) -> Value {
        let runs = self.get(who, "/api/tasks/activity").await;
        find(&runs, "id", run)["dismissal"].clone()
    }

    async fn dismiss(&self, task: &str, action: Value) -> (StatusCode, Value) {
        self.request(
            &self.owner,
            "POST",
            "/api/dismissals",
            json!({ "taskId": task, "action": action }),
        )
        .await
    }

    async fn read(&self, who: &RelayContext, items: &[Value]) -> (StatusCode, Value) {
        self.request(who, "POST", "/api/reads", json!({ "items": items }))
            .await
    }

    /// Records a run that finished now, with the agent's final message.
    async fn finished_run(&self, task: &str, trigger: &str, status: RunStatus) -> String {
        // Finish strictly after any read mark taken in the same millisecond.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let run = id();
        let record = json!({
            "id": run,
            "taskId": task,
            "status": status,
            "trigger": trigger,
            "createdAt": now(),
            "finishedAt": now(),
            "summary": "## Release ready\n\nI **shipped** the `0.53` notes.",
        });
        self.service
            .store
            .transaction(move |db| {
                db.add_run_fixture(&record, None)?;
                Ok(())
            })
            .await
            .unwrap();
        run
    }

    /// A conversation whose agent just finished a turn.
    async fn finished_chat(&self) -> Value {
        let (_, chat) = self
            .request(&self.owner, "POST", "/api/chats", json!({}))
            .await;
        let run = self
            .finished_run(text(&chat, "id"), "chat", RunStatus::Succeeded)
            .await;
        let mut stored = self
            .service
            .conversation_boundary(text(&chat, "id"))
            .await
            .unwrap();
        stored["runId"] = run.into();
        self.service
            .store
            .save_conversation_boundary(stored)
            .await
            .unwrap();
        chat
    }
}

fn chat_read(id: &str, finished_at: &Value) -> Value {
    json!({ "target": { "kind": "chat", "chatId": id }, "finishedAt": finished_at })
}

fn mission_read(id: &str, finished_at: &Value) -> Value {
    json!({ "target": { "kind": "mission", "taskId": id }, "finishedAt": finished_at })
}

fn find<'a>(list: &'a Value, key: &str, id: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|item| item[key] == id)
        .unwrap()
}

#[tokio::test]
async fn finished_conversations_stay_unread_for_each_account_until_opened() {
    let app = App::new().await;
    // Both accounts start reading before the work finishes: earlier history counts as read.
    app.get(&app.owner, "/api/chats").await;
    app.get(&app.member, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");

    for who in [&app.owner, &app.member] {
        let listed = app.get(who, "/api/chats").await;
        let item = find(&listed, "id", chat_id);
        assert_eq!(item["unread"], true, "{item}");
        assert_eq!(item["status"], "succeeded");
        assert!(item["finishedAt"].as_i64().is_some());
        assert_eq!(item["preview"], "Release ready. I shipped the 0.53 notes.");
    }

    let finished_at =
        find(&app.get(&app.owner, "/api/chats").await, "id", chat_id)["finishedAt"].clone();
    let (status, marked) = app
        .read(&app.owner, &[chat_read(chat_id, &finished_at)])
        .await;
    assert_eq!(status, StatusCode::OK, "{marked}");
    assert_eq!(marked["read"], 1);
    let owner = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&owner, "id", chat_id)["unread"], false);
    assert!(find(&owner, "id", chat_id)["preview"].is_null());
    // The member has not opened it yet.
    let member = app.get(&app.member, "/api/chats").await;
    assert_eq!(find(&member, "id", chat_id)["unread"], true);

    // The agent's next turn is new again.
    let run = app
        .finished_run(chat_id, "chat", RunStatus::Succeeded)
        .await;
    let mut stored = app.service.conversation_boundary(chat_id).await.unwrap();
    stored["runId"] = run.into();
    app.service
        .store
        .save_conversation_boundary(stored)
        .await
        .unwrap();
    let owner = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&owner, "id", chat_id)["unread"], true);
}

#[tokio::test]
async fn work_finished_before_an_account_first_reads_is_not_unread() {
    let app = App::new().await;
    let chat = app.finished_chat().await;
    let listed = app.get(&app.member, "/api/chats").await;
    assert_eq!(find(&listed, "id", text(&chat, "id"))["unread"], false);
}

#[tokio::test]
async fn plugin_conversations_answer_in_their_channel_and_are_never_unread() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    let chat = app.finished_chat().await;
    app.service
        .store
        .set(
            &format!("plugin-conversation:{}", text(&chat, "id")),
            json!({ "connectionId": "discord", "thread": "support" }),
            None,
        )
        .await
        .unwrap();
    let listed = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&listed, "id", text(&chat, "id"))["unread"], false);
}

#[tokio::test]
async fn failed_and_stopped_work_is_never_unread() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    let (_, chat) = app
        .request(&app.owner, "POST", "/api/chats", json!({}))
        .await;
    for status in [RunStatus::Failed, RunStatus::Cancelled] {
        let run = app.finished_run(text(&chat, "id"), "chat", status).await;
        let mut stored = app
            .service
            .conversation_boundary(text(&chat, "id"))
            .await
            .unwrap();
        stored["runId"] = run.into();
        app.service
            .store
            .save_conversation_boundary(stored)
            .await
            .unwrap();
        let listed = app.get(&app.owner, "/api/chats").await;
        assert_eq!(find(&listed, "id", text(&chat, "id"))["unread"], false);
    }
}

#[tokio::test]
async fn mission_runs_are_unread_until_their_mission_is_opened() {
    let app = App::new().await;
    let task = app
        .service
        .task_fixture(
            json!({ "name": "Release notes", "prompt": "Write them", "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let task_id = text(&task, "id");
    app.get(&app.owner, "/api/tasks/activity").await;
    let run = app
        .finished_run(task_id, "schedule", RunStatus::Succeeded)
        .await;

    let runs = app.get(&app.owner, "/api/tasks/activity").await;
    let item = find(&runs, "id", &run);
    assert_eq!(item["unread"], true, "{item}");
    assert_eq!(item["preview"], "Release ready. I shipped the 0.53 notes.");
    // An account that starts reading afterwards does not inherit it.
    let runs = app.get(&app.member, "/api/tasks/activity").await;
    assert_eq!(find(&runs, "id", &run)["unread"], false);

    let (status, _) = app
        .read(&app.owner, &[mission_read(task_id, &item["finishedAt"])])
        .await;
    assert_eq!(status, StatusCode::OK);
    let runs = app.get(&app.owner, "/api/tasks/activity").await;
    assert_eq!(find(&runs, "id", &run)["unread"], false);
}

#[tokio::test]
async fn marking_as_read_accepts_only_conversations_and_missions() {
    let app = App::new().await;
    let at = json!(now() - 1000);
    for items in [
        json!([]),
        json!([{ "target": { "kind": "agent", "agentId": id() }, "finishedAt": at }]),
        json!([chat_read("not-a-uuid", &at)]),
        json!([format!("chat:{}", id())]),
        json!([chat_read(&id(), &json!(0))]),
        json!([chat_read(&id(), &json!(now() + 60_000))]),
        json!([{ "target": { "kind": "chat", "chatId": id() } }]),
        json!(vec![chat_read(&id(), &at); 201]),
    ] {
        let (status, body) = app
            .request(&app.member, "POST", "/api/reads", json!({ "items": items }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{items}: {body}");
    }
    let (status, body) = app
        .read(
            &app.member,
            &[chat_read(&id(), &at), mission_read(&id(), &at)],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["read"], 2);
}

#[tokio::test]
async fn reading_an_observed_completion_does_not_consume_newer_work() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");
    let listed = app.get(&app.owner, "/api/chats").await;
    let first_at = find(&listed, "id", chat_id)["finishedAt"].as_i64().unwrap();
    let run = app
        .finished_run(chat_id, "chat", RunStatus::Succeeded)
        .await;
    let mut stored = app.service.conversation_boundary(chat_id).await.unwrap();
    stored["runId"] = run.into();
    app.service
        .store
        .save_conversation_boundary(stored)
        .await
        .unwrap();

    let read = |at: i64| json!({ "items": [chat_read(chat_id, &json!(at))] });
    let (status, _) = app
        .request(&app.owner, "POST", "/api/reads", read(first_at))
        .await;
    assert_eq!(status, StatusCode::OK);
    let listed = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&listed, "id", chat_id)["unread"], true);
    let latest_at = find(&listed, "id", chat_id)["finishedAt"].as_i64().unwrap();

    let (status, _) = app
        .request(&app.owner, "POST", "/api/reads", read(latest_at))
        .await;
    assert_eq!(status, StatusCode::OK);
    // An older request finishing later never lowers the watermark.
    app.request(&app.owner, "POST", "/api/reads", read(first_at))
        .await;
    let listed = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&listed, "id", chat_id)["unread"], false);
}

#[tokio::test]
async fn opening_an_old_mission_run_leaves_the_latest_result_unread() {
    let app = App::new().await;
    let task = app
        .service
        .task_fixture(
            json!({
                "name": "Release notes",
                "prompt": "Write them",
                "agentId": MAIN_AGENT_ID,
            }),
            None,
        )
        .await
        .unwrap();
    let task_id = text(&task, "id");
    app.get(&app.owner, "/api/tasks/activity").await;
    let first = app
        .finished_run(task_id, "schedule", RunStatus::Succeeded)
        .await;
    let old = app.service.store.run(&first).await.unwrap();
    let latest = app
        .finished_run(task_id, "schedule", RunStatus::Succeeded)
        .await;
    let (status, _) = app
        .request(
            &app.owner,
            "POST",
            "/api/reads",
            json!({ "items": [mission_read(task_id, &old["finishedAt"])] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let runs = app.get(&app.owner, "/api/tasks/activity").await;
    assert_eq!(find(&runs, "id", &latest)["unread"], true);
}

#[tokio::test]
async fn the_live_conversation_list_follows_each_subscribers_reads() {
    let app = App::new().await;
    let (url, server) = common::serve_locally(app.router.clone()).await;
    app.get(&app.owner, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id").to_owned();

    let token = app.owner.token().to_owned();
    let mut response = reqwest::Client::new()
        .get(format!("{url}/api/chats/stream"))
        .header("x-test-relay-token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut received = String::new();
    let mut unread_states = Vec::new();
    let marker = format!("\"id\":\"{chat_id}\"");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let chunk = response.chunk().await.unwrap().expect("stream ended");
            received.push_str(std::str::from_utf8(&chunk).unwrap());
            while let Some(end) = received.find("\n\n") {
                let frame: String = received.drain(..end + 2).collect();
                let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) else {
                    continue;
                };
                if !data.contains(&marker) {
                    continue;
                }
                let batch: Value = serde_json::from_str(data).unwrap();
                let chats = &batch["state"]["chats"];
                if chats.is_array() {
                    unread_states.push(find(chats, "id", &chat_id)["unread"].clone());
                }
                if unread_states.len() == 1 {
                    let finished_at = find(chats, "id", &chat_id)["finishedAt"].clone();
                    app.read(&app.owner, &[chat_read(&chat_id, &finished_at)])
                        .await;
                }
            }
            if unread_states.len() == 2 {
                return;
            }
        }
    })
    .await
    .expect("the read mark reached the stream");
    assert_eq!(unread_states, [json!(true), json!(false)]);
    server.abort();
}

#[tokio::test]
async fn blocked_missions_stay_set_aside_for_each_account_until_they_finish_again() {
    let app = App::new().await;
    let task = app
        .service
        .task_fixture(
            json!({ "name": "Device proof", "prompt": "Prove it", "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let task_id = text(&task, "id");
    let blocked =
        |status: &str| json!({ "outcome": { "status": status, "reason": "CI is the parent's." } });
    let run = app.finished_run(task_id, "mcp", RunStatus::Succeeded).await;
    let stored = app
        .service
        .store
        .change_run_fixture(&run, blocked("blocked"))
        .await
        .unwrap();
    let finished_at = stored["finishedAt"].as_i64().unwrap();
    assert!(app.dismissal(&app.owner, &run).await.is_null());

    let (status, body) = app
        .dismiss(
            task_id,
            json!({ "kind": "dismiss", "finishedAt": finished_at }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        app.dismissal(&app.owner, &run).await,
        json!({ "until": null })
    );
    // Each member sets work aside on their own.
    assert!(app.dismissal(&app.member, &run).await.is_null());

    // Input the agent asked for can only be snoozed.
    app.service
        .store
        .change_run_fixture(&run, blocked("needs_input"))
        .await
        .unwrap();
    assert!(app.dismissal(&app.owner, &run).await.is_null());
    let until = now() + 60_000;
    let (_, snoozed) = app
        .dismiss(
            task_id,
            json!({ "kind": "snooze", "finishedAt": finished_at, "until": until }),
        )
        .await;
    assert_eq!(
        snoozed,
        json!({ "taskId": task_id, "dismissal": { "until": until } })
    );
    assert_eq!(
        app.dismissal(&app.owner, &run).await,
        json!({ "until": until })
    );

    let (status, restored) = app.dismiss(task_id, json!({ "kind": "restore" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(restored, json!({ "taskId": task_id, "dismissal": null }));
    assert!(app.dismissal(&app.owner, &run).await.is_null());

    // A later finish asks again.
    app.dismiss(
        task_id,
        json!({ "kind": "dismiss", "finishedAt": finished_at }),
    )
    .await;
    let next = app.finished_run(task_id, "mcp", RunStatus::Succeeded).await;
    app.service
        .store
        .change_run_fixture(&next, blocked("blocked"))
        .await
        .unwrap();
    let runs = app.get(&app.owner, "/api/tasks/activity").await;
    assert!(find(&runs, "id", &next)["dismissal"].is_null());

    for action in [
        json!({ "kind": "dismiss" }),
        json!({ "kind": "dismiss", "finishedAt": 0 }),
        json!({ "kind": "hide", "finishedAt": finished_at }),
        json!({ "kind": "snooze", "finishedAt": finished_at, "until": now() - 1 }),
        json!({ "kind": "snooze", "finishedAt": finished_at, "until": now() + 31 * 24 * 60 * 60 * 1000 }),
    ] {
        let (status, body) = app.dismiss(task_id, action.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{action}: {body}");
    }
    let (status, body) = app
        .dismiss("not-a-task", json!({ "kind": "restore" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

async fn tool(app: &Router, endpoint: &str, bearer: &str, name: &str, input: Value) -> Value {
    let request = common::request("POST", endpoint)
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": common::mcp_arguments(name, &input),
            })
            .to_string(),
        ))
        .unwrap();
    common::read_json(common::send(app, request).await).await
}

fn result(response: &Value) -> &Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    assert!(response["error"].is_null(), "{response}");
    &response["result"]["structuredContent"]["result"]
}

/// A running execution of an agent granted installation access.
async fn installation_token(s: &Arc<Service>) -> String {
    use common::AgentFixture;
    std::fs::create_dir_all(&s.config.home).unwrap();
    let agent = s
        .agent_fixture(
            json!({ "name": "Manager", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let task = s
        .task_fixture(
            json!({ "name": "Caller", "prompt": "Manage", "agentId": agent["id"] }),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    s.store
        .change_run(
            text(&run, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let run = s.store.run(text(&run, "id")).await.unwrap();
    let config = s.mcps.run_configuration(s, &run).await.unwrap();
    config["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn personal_state_follows_the_authorizing_account_and_never_an_execution() {
    let app = App::new().await;
    let execution = installation_token(&app.service).await;
    let public = common::relay_fixture::grant(&app.service, &["read", "manage"]).await;
    app.get(&app.owner, "/api/chats").await;
    app.get(&app.member, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");
    let page = json!({ "view": "active", "limit": 100, "offset": 0 });

    // The public grant belongs to the owner who authorized it.
    let listed = tool(&app.router, "/mcp", &public, "list_chats", page.clone()).await;
    let item = find(&result(&listed)["chats"], "id", chat_id).clone();
    assert_eq!(item["unread"], true, "{item}");
    assert_eq!(item["preview"], "Release ready. I shipped the 0.53 notes.");
    // An execution has no account: nothing is unread for it, and nothing borrowed.
    let listed = tool(
        &app.router,
        "/mcp-installation",
        &execution,
        "list_chats",
        page.clone(),
    )
    .await;
    let unowned = find(&result(&listed)["chats"], "id", chat_id);
    assert_eq!(unowned["unread"], false, "{unowned}");
    assert!(unowned["preview"].is_null());

    let mark = json!({ "items": [chat_read(chat_id, &item["finishedAt"])] });
    let refused = tool(
        &app.router,
        "/mcp-installation",
        &execution,
        "mark_read",
        mark.clone(),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(refused.to_string().contains("no delegation"), "{refused}");
    let readonly = common::relay_fixture::grant(&app.service, &["read"]).await;
    let refused = tool(&app.router, "/mcp", &readonly, "mark_read", mark.clone()).await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    for who in [&app.owner, &app.member] {
        let listed = app.get(who, "/api/chats").await;
        assert_eq!(find(&listed, "id", chat_id)["unread"], true);
    }

    let marked = tool(&app.router, "/mcp", &public, "mark_read", mark).await;
    assert_eq!(result(&marked)["read"], 1, "{marked}");
    let owner = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&owner, "id", chat_id)["unread"], false);
    let member = app.get(&app.member, "/api/chats").await;
    assert_eq!(find(&member, "id", chat_id)["unread"], true);
}

#[tokio::test]
async fn mission_outcomes_set_aside_on_a_tool_stay_with_its_account() {
    let app = App::new().await;
    let execution = installation_token(&app.service).await;
    let public = common::relay_fixture::grant(&app.service, &["read", "manage"]).await;
    let task = app
        .service
        .task_fixture(
            json!({ "name": "Device proof", "prompt": "Prove it", "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let task_id = text(&task, "id");
    for who in [&app.owner, &app.member] {
        app.get(who, "/api/tasks/activity").await;
    }
    let run = app.finished_run(task_id, "mcp", RunStatus::Succeeded).await;
    let stored = app
        .service
        .store
        .change_run_fixture(
            &run,
            json!({ "outcome": { "status": "blocked", "reason": "Waiting for review." } }),
        )
        .await
        .unwrap();
    let dismiss = json!({
        "taskId": task_id,
        "action": { "kind": "dismiss", "finishedAt": stored["finishedAt"] },
    });

    let refused = tool(
        &app.router,
        "/mcp-installation",
        &execution,
        "set_outcome_aside",
        dismiss.clone(),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(refused.to_string().contains("no delegation"), "{refused}");
    let set = tool(&app.router, "/mcp", &public, "set_outcome_aside", dismiss).await;
    assert_eq!(
        result(&set),
        &json!({ "taskId": task_id, "dismissal": { "until": null } })
    );

    // Each surface lists the latest run with its own caller's state; executions get none.
    assert_eq!(
        app.dismissal(&app.owner, &run).await,
        json!({ "until": null })
    );
    assert!(app.dismissal(&app.member, &run).await.is_null());
    let activity = tool(
        &app.router,
        "/mcp",
        &public,
        "list_task_activity",
        json!({}),
    )
    .await;
    let listed = find(result(&activity), "id", &run);
    assert_eq!(listed["dismissal"], json!({ "until": null }));
    assert_eq!(listed["unread"], true);
    let activity = tool(
        &app.router,
        "/mcp-installation",
        &execution,
        "list_task_activity",
        json!({}),
    )
    .await;
    let listed = find(result(&activity), "id", &run);
    assert!(listed["dismissal"].is_null(), "{listed}");
    assert_eq!(listed["unread"], false);
}
