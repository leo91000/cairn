use crate::common::ChatRunFixture;
use crate::common::run_fixture::DbRunFixture;
use crate::common::run_fixture::RunFixture;

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
use std::{collections::HashSet, sync::Arc, time::Duration};
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

    async fn snooze(&self, who: &RelayContext, chat: &str, action: Value) -> (StatusCode, Value) {
        self.request(
            who,
            "POST",
            "/api/snoozes",
            json!({ "chatId": chat, "action": action }),
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
async fn plugin_conversations_answer_in_their_channel_and_surface_reminders_and_blocked_outcomes() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");
    app.service
        .store
        .set(
            &format!("plugin-conversation:{chat_id}"),
            json!({ "connectionId": "discord", "thread": "support" }),
            None,
        )
        .await
        .unwrap();
    let listed = app.get(&app.owner, "/api/chats").await;
    let quiet = find(&listed, "id", chat_id);
    assert_eq!(quiet["unread"], false);
    let until = now() + 60_000;
    let (status, snoozed) = app
        .snooze(
            &app.owner,
            chat_id,
            json!({ "kind": "snooze", "finishedAt": quiet["readAt"], "until": until }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{snoozed}");
    assert_eq!(snoozed["snoozedUntil"], until);
    let (status, result) = app
        .snooze(&app.owner, chat_id, json!({ "kind": "restore" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let listed = app.get(&app.owner, "/api/chats").await;
    let reminder = find(&listed, "id", chat_id);
    assert_eq!(reminder["unread"], true);
    let (status, marked) = app
        .read(&app.owner, &[chat_read(chat_id, &reminder["readAt"])])
        .await;
    assert_eq!(status, StatusCode::OK, "{marked}");
    tokio::time::sleep(Duration::from_millis(5)).await;
    app.service
        .store
        .change_run_fixture(
            text(reminder, "runId"),
            json!({
                "finishedAt": now(),
                "outcome": { "status": "blocked", "reason": "Needs a person." },
            }),
        )
        .await
        .unwrap();
    let listed = app.get(&app.owner, "/api/chats").await;
    let blocked = find(&listed, "id", chat_id);
    assert_eq!(blocked["unread"], true);
    assert!(blocked["snoozedUntil"].is_null());
}

#[tokio::test]
async fn errors_can_be_read_without_changing_their_status() {
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
        let item = find(&listed, "id", text(&chat, "id"));
        assert_eq!(item["unread"], status == RunStatus::Failed);
        assert_eq!(
            app.read(&app.owner, &[chat_read(text(&chat, "id"), &item["readAt"])])
                .await
                .0,
            StatusCode::OK
        );
        let listed = app.get(&app.owner, "/api/chats").await;
        let read = find(&listed, "id", text(&chat, "id"));
        assert_eq!(read["unread"], false);
        assert_eq!(read["status"], item["status"]);
    }
}

#[tokio::test]
async fn marking_as_read_accepts_only_conversations() {
    let app = App::new().await;
    let at = json!(now() - 1000);
    for items in [
        json!([]),
        json!([{ "target": { "kind": "agent", "agentId": id() }, "finishedAt": at }]),
        json!([{ "target": { "kind": "mission", "taskId": id() }, "finishedAt": at }]),
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
        .read(&app.member, &[chat_read(&id(), &at), chat_read(&id(), &at)])
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
async fn conversations_can_be_snoozed_restored_and_cleared_by_mark_read() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    app.get(&app.member, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");
    let run_id = app.service.conversation_boundary(chat_id).await.unwrap()["runId"]
        .as_str()
        .unwrap()
        .to_owned();
    let stored = app
        .service
        .store
        .change_run_fixture(
            &run_id,
            json!({ "outcome": { "status": "blocked", "reason": "Waiting on deployment approval." } }),
        )
        .await
        .unwrap();
    let finished_at = stored["finishedAt"].as_i64().unwrap();

    let listed = app.get(&app.owner, "/api/chats").await;
    let item = find(&listed, "id", chat_id);
    assert_eq!(item["outcome"]["status"], "blocked");
    assert!(item["snoozedUntil"].is_null());
    assert_eq!(item["unread"], true);

    let until = now() + 60_000;
    let (status, snoozed) = app
        .snooze(
            &app.owner,
            chat_id,
            json!({ "kind": "snooze", "finishedAt": finished_at, "until": until }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{snoozed}");
    assert_eq!(snoozed, json!({ "chatId": chat_id, "snoozedUntil": until }));

    let (status, stale) = app
        .snooze(
            &app.owner,
            chat_id,
            json!({ "kind": "snooze", "finishedAt": finished_at - 1, "until": until + 60_000 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{stale}");
    assert_eq!(stale["snoozedUntil"], until);

    // Owner sees snoozedUntil and unread=false while snooze is active; member is unaffected.
    let owner_list = app.get(&app.owner, "/api/chats").await;
    let owner_item = find(&owner_list, "id", chat_id);
    assert_eq!(owner_item["snoozedUntil"], until);
    assert_eq!(owner_item["unread"], false);
    let member_list = app.get(&app.member, "/api/chats").await;
    assert!(find(&member_list, "id", chat_id)["snoozedUntil"].is_null());
    assert_eq!(find(&member_list, "id", chat_id)["unread"], true);

    // Restore brings it back to For you.
    let (status, restored) = app
        .snooze(&app.owner, chat_id, json!({ "kind": "restore" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored, json!({ "chatId": chat_id, "snoozedUntil": null }));
    let owner_list = app.get(&app.owner, "/api/chats").await;
    let reminder = find(&owner_list, "id", chat_id);
    assert_eq!(reminder["readAt"], reminder["snoozedUntil"]);
    assert!(reminder["readAt"].as_i64().unwrap() < until);
    assert_eq!(reminder["unread"], true);

    // Mark read clears any active snooze ("écarter devient marquer lu").
    app.snooze(
        &app.owner,
        chat_id,
        json!({ "kind": "snooze", "finishedAt": finished_at, "until": until }),
    )
    .await;
    let (status, _) = app
        .read(&app.owner, &[chat_read(chat_id, &json!(finished_at))])
        .await;
    assert_eq!(status, StatusCode::OK);
    let owner_list = app.get(&app.owner, "/api/chats").await;
    let owner_item = find(&owner_list, "id", chat_id);
    assert!(owner_item["snoozedUntil"].is_null());
    assert_eq!(owner_item["unread"], false);

    // A Restore sent by another device after Mark read has nothing left to restore.
    assert_eq!(
        app.snooze(&app.owner, chat_id, json!({ "kind": "restore" }))
            .await
            .0,
        StatusCode::OK
    );
    let owner_list = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&owner_list, "id", chat_id)["unread"], false);

    // Validation checks on /api/snoozes.
    for action in [
        json!({ "kind": "snooze" }),
        json!({ "kind": "snooze", "finishedAt": 0, "until": until }),
        json!({ "kind": "dismiss", "finishedAt": finished_at }),
        json!({ "kind": "snooze", "finishedAt": finished_at, "until": now() - 1 }),
        json!({ "kind": "snooze", "finishedAt": finished_at, "until": now() + 31 * 24 * 60 * 60 * 1000 }),
    ] {
        let (status, body) = app.snooze(&app.owner, chat_id, action.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{action}: {body}");
    }
    let (status, body) = app
        .snooze(&app.owner, "not-a-chat", json!({ "kind": "restore" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn restoring_and_expiring_a_snooze_both_remind_the_account_of_an_idle_conversation() {
    let app = App::new().await;
    let (status, chat) = app
        .request(&app.owner, "POST", "/api/chats", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    let chat_id = text(&chat, "id");

    for restore in [true, false] {
        let listed = app.get(&app.owner, "/api/chats").await;
        let observed = find(&listed, "id", chat_id);
        let activity = &observed["updatedAt"];
        let until = now() + 60_000;
        let (status, snoozed) = app
            .snooze(
                &app.owner,
                chat_id,
                json!({ "kind": "snooze", "finishedAt": activity, "until": until }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{snoozed}");
        assert_eq!(snoozed["snoozedUntil"], until);
        tokio::time::sleep(Duration::from_millis(5)).await;
        if restore {
            let (status, restored) = app
                .snooze(&app.owner, chat_id, json!({ "kind": "restore" }))
                .await;
            assert_eq!(status, StatusCode::OK, "{restored}");
        } else {
            let chat_id = chat_id.to_owned();
            app.service
                .store
                .transaction(move |db| {
                    db.0.execute(
                        "UPDATE account_snoozes SET snoozed_until=?1 WHERE chat_id=?2",
                        rusqlite::params![now(), chat_id],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }

        let listed = app.get(&app.owner, "/api/chats").await;
        let reminder = find(&listed, "id", chat_id);
        assert_eq!(reminder["status"], "idle");
        assert_eq!(reminder["unread"], true, "{reminder}");
        assert_eq!(reminder["readAt"], reminder["snoozedUntil"]);

        let observed_at = &reminder["readAt"];
        let until = now() + 60_000;
        let (status, snoozed) = app
            .snooze(
                &app.owner,
                chat_id,
                json!({ "kind": "snooze", "finishedAt": observed_at, "until": until }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{snoozed}");
        let listed = app.get(&app.owner, "/api/chats").await;
        let snoozed = find(&listed, "id", chat_id);
        assert_eq!(snoozed["readAt"], *observed_at);
        assert_eq!(snoozed["snoozedUntil"], until);
        assert_eq!(snoozed["unread"], false);

        assert_eq!(
            app.read(&app.owner, &[chat_read(chat_id, &snoozed["readAt"])])
                .await
                .0,
            StatusCode::OK,
        );
        let listed = app.get(&app.owner, "/api/chats").await;
        let cleared = find(&listed, "id", chat_id);
        assert_eq!(cleared["unread"], false);
        assert!(cleared["snoozedUntil"].is_null());
        let member = app.get(&app.member, "/api/chats").await;
        assert_eq!(find(&member, "id", chat_id)["unread"], false);
    }
}

#[tokio::test]
async fn preparation_failure_surfaces_after_an_acknowledged_and_snoozed_turn() {
    let app = App::new().await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");
    let listed = app.get(&app.owner, "/api/chats").await;
    let previous = find(&listed, "id", chat_id);
    let previous_at = &previous["readAt"];
    let (status, mark) = app
        .read(&app.owner, &[chat_read(chat_id, previous_at)])
        .await;
    assert_eq!(status, StatusCode::OK, "{mark}");
    let until = now() + 60_000;
    let action = json!({ "kind": "snooze", "finishedAt": previous_at, "until": until });
    let (status, snoozed) = app.snooze(&app.owner, chat_id, action.clone()).await;
    assert_eq!(status, StatusCode::OK, "{snoozed}");
    assert_eq!(snoozed["snoozedUntil"], until);

    tokio::time::sleep(Duration::from_millis(5)).await;
    // The fixture has no managed Codex home for preparing this next turn.
    let (status, sent) = app
        .request(
            &app.owner,
            "POST",
            &format!("/api/chats/{chat_id}/messages"),
            json!({ "id": id(), "text": "Please follow up." }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    app.service.chat_tick(&HashSet::new()).await.unwrap();
    let listed = app.get(&app.owner, "/api/chats").await;
    let failed = find(&listed, "id", chat_id);
    assert_eq!(failed["runId"], previous["runId"]);
    assert_eq!(failed["finishedAt"], previous["finishedAt"]);
    assert!(failed["readAt"].as_i64().unwrap() > previous_at.as_i64().unwrap());
    assert!(failed["error"].is_string(), "{failed}");
    assert!(failed["snoozedUntil"].is_null(), "{failed}");
    assert_eq!(failed["unread"], true, "{failed}");

    let (status, delayed) = app.snooze(&app.owner, chat_id, action).await;
    assert_eq!(status, StatusCode::OK, "{delayed}");
    assert!(delayed["snoozedUntil"].is_null(), "{delayed}");
}

#[tokio::test]
async fn a_queued_followup_ends_a_snooze_without_marking_a_read_result_unread() {
    let app = App::new().await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");
    let listed = app.get(&app.owner, "/api/chats").await;
    let previous = find(&listed, "id", chat_id);
    let (_, marked) = app
        .read(&app.owner, &[chat_read(chat_id, &previous["readAt"])])
        .await;
    assert_eq!(marked["read"], 1);
    let until = now() + 60_000;
    let action = json!({ "kind": "snooze", "finishedAt": previous["readAt"], "until": until });
    let (status, snoozed) = app.snooze(&app.owner, chat_id, action.clone()).await;
    assert_eq!(status, StatusCode::OK, "{snoozed}");
    assert_eq!(snoozed["snoozedUntil"], until);

    tokio::time::sleep(Duration::from_millis(5)).await;
    let (status, sent) = app
        .request(
            &app.owner,
            "POST",
            &format!("/api/chats/{chat_id}/messages"),
            json!({ "id": id(), "text": "Please verify the totals." }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let listed = app.get(&app.owner, "/api/chats").await;
    let current = find(&listed, "id", chat_id);
    assert_eq!(current["finishedAt"], previous["finishedAt"]);
    assert_eq!(current["readAt"], previous["readAt"]);
    assert_eq!(current["unread"], false);
    assert!(current["snoozedUntil"].is_null());

    let (status, stale) = app.snooze(&app.owner, chat_id, action).await;
    assert_eq!(status, StatusCode::OK, "{stale}");
    assert!(stale["snoozedUntil"].is_null());
    let (status, latest) = app
        .snooze(
            &app.owner,
            chat_id,
            json!({ "kind": "snooze", "finishedAt": current["lastActivityAt"], "until": until }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{latest}");
    assert_eq!(latest["snoozedUntil"], until);
}

#[tokio::test]
async fn snoozing_a_previous_observation_leaves_a_new_turn_unread() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");
    let listed = app.get(&app.owner, "/api/chats").await;
    let observed = find(&listed, "id", chat_id);
    let run_id = text(observed, "runId");
    let previous = observed["readAt"].as_i64().unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    app.service
        .store
        .change_run_fixture(run_id, json!({ "finishedAt": now() }))
        .await
        .unwrap();

    let (status, result) = app
        .snooze(
            &app.owner,
            chat_id,
            json!({ "kind": "snooze", "finishedAt": previous, "until": now() + 60_000 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(result["snoozedUntil"].is_null());
    let chats = app.get(&app.owner, "/api/chats").await;
    let current = find(&chats, "id", chat_id);
    assert!(current["snoozedUntil"].is_null());
    assert_eq!(current["unread"], true);
}

#[tokio::test]
async fn rule_started_conversations_stay_quiet_unless_blocked_or_a_person_writes() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id").to_owned();
    let rule_id = id();

    // Attach chat as started by a clock rule without human participation.
    let chat_id_for_db = chat_id.clone();
    let rule_id_for_db = rule_id.clone();
    app.service
        .store
        .transaction(move |db| {
            db.0.execute(
                "INSERT INTO hook_rules(id,creator_account_id,creator_access_id,creator_owner,revision,version,created_at,updated_at) \
                 VALUES(?1,'owner-account','owner-access',1,'revision',1,?2,?2)",
                rusqlite::params![rule_id_for_db, now()],
            )?;
            db.0.execute(
                "INSERT INTO hook_rule_conversations(chat_id,creator_account_id,rule_id,agent_id,origin,created_at) \
                 VALUES(?1,'owner-account',?2,?3,?4,?5)",
                rusqlite::params![
                    chat_id_for_db,
                    rule_id_for_db,
                    MAIN_AGENT_ID,
                    json!({ "ruleName": "Morning digest", "schedule": true }).to_string(),
                    now(),
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Quiet on ordinary completion.
    let listed = app.get(&app.owner, "/api/chats").await;
    let item = find(&listed, "id", &chat_id);
    assert_eq!(item["originRule"]["ruleId"], rule_id);
    assert_eq!(item["originRule"]["ruleName"], "Morning digest");
    assert_eq!(item["originRule"]["schedule"], true);
    assert_eq!(item["personParticipated"], false);
    assert_eq!(
        item["unread"], false,
        "quiet rule chat must not be unread: {item}"
    );

    // Surfaces when blocked waiting on user.
    let run_id = app.service.conversation_boundary(&chat_id).await.unwrap()["runId"]
        .as_str()
        .unwrap()
        .to_owned();
    app.service
        .store
        .change_run_fixture(
            &run_id,
            json!({ "outcome": { "status": "blocked", "reason": "Needs review." } }),
        )
        .await
        .unwrap();
    let listed = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&listed, "id", &chat_id)["unread"], true);

    // First-read history suppression still lets a new reader see unresolved work.
    let first_visit = app.get(&app.member, "/api/chats").await;
    assert_eq!(find(&first_visit, "id", &chat_id)["unread"], true);

    // A blocked outcome also surfaces while an outside check runs.
    app.service
        .store
        .change_run_fixture(
            &run_id,
            json!({ "outcome": { "status": "blocked", "reason": "CI running", "waitingOn": "external" } }),
        )
        .await
        .unwrap();
    let listed = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&listed, "id", &chat_id)["unread"], true);

    // Once a person participates, even an ordinary completion becomes unread.
    app.service
        .store
        .change_run_fixture(
            &run_id,
            json!({ "outcome": { "status": "completed", "reason": "Done." } }),
        )
        .await
        .unwrap();
    let message = id();
    let (status, sent) = app
        .request(
            &app.owner,
            "POST",
            &format!("/api/chats/{chat_id}/messages"),
            json!({ "id": message, "text": "Please continue the review." }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    app.service
        .store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE chat_messages SET status='delivered' WHERE id=?",
                [&message],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let listed = app.get(&app.owner, "/api/chats").await;
    let item = find(&listed, "id", &chat_id);
    assert_eq!(item["personParticipated"], true);
    assert_eq!(item["unread"], true);
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
    let (_chat, run) = s
        .chat_run_fixture(text(&agent, "id"), None, "Manage")
        .await
        .unwrap();
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
async fn conversation_snoozes_on_a_tool_stay_with_its_account() {
    let app = App::new().await;
    let execution = installation_token(&app.service).await;
    let public = common::relay_fixture::grant(&app.service, &["read", "manage"]).await;
    app.get(&app.owner, "/api/chats").await;
    app.get(&app.member, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id");
    let finished_at =
        find(&app.get(&app.owner, "/api/chats").await, "id", chat_id)["finishedAt"].clone();
    let until = now() + 60_000;
    let snooze = json!({
        "chatId": chat_id,
        "action": { "kind": "snooze", "finishedAt": finished_at, "until": until },
    });

    let refused = tool(
        &app.router,
        "/mcp-installation",
        &execution,
        "snooze_chat",
        snooze.clone(),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(refused.to_string().contains("no delegation"), "{refused}");
    let set = tool(&app.router, "/mcp", &public, "snooze_chat", snooze).await;
    assert_eq!(
        result(&set),
        &json!({ "chatId": chat_id, "snoozedUntil": until })
    );

    let owner = app.get(&app.owner, "/api/chats").await;
    assert_eq!(find(&owner, "id", chat_id)["snoozedUntil"], until);
    let member = app.get(&app.member, "/api/chats").await;
    assert!(find(&member, "id", chat_id)["snoozedUntil"].is_null());

    let page = json!({ "view": "active", "limit": 100, "offset": 0 });
    let public_chats = tool(&app.router, "/mcp", &public, "list_chats", page.clone()).await;
    assert_eq!(
        find(&result(&public_chats)["chats"], "id", chat_id)["snoozedUntil"],
        until
    );
    let exec_chats = tool(
        &app.router,
        "/mcp-installation",
        &execution,
        "list_chats",
        page,
    )
    .await;
    assert!(find(&result(&exec_chats)["chats"], "id", chat_id)["snoozedUntil"].is_null());
}

#[tokio::test]
async fn reading_a_pending_question_does_not_answer_it_and_new_questions_notify_again() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    app.get(&app.member, "/api/chats").await;
    let chat = app.finished_chat().await;
    let chat_id = text(&chat, "id").to_owned();
    for index in 0..2 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let chat_id = chat_id.clone();
        app.service.store.transaction(move |db| {
            let chat = db.conversation(&chat_id)?.unwrap();
            db.update_run_boundary_fixture(chat.run_id.as_deref().unwrap(), &json!({ "status": "running", "finishedAt": null }))?;
            db.save_question(&serde_json::from_value(json!({
                "id": format!("question-{index}"), "chatId": chat_id, "runId": chat.run_id,
                "source": "cairn", "status": "pending", "blocking": false, "createdAt": now(),
                "fields": [{ "id": "field", "title": "Pick one", "mode": "single", "secret": false, "options": [] }],
            }))?)?;
            Ok(())
        }).await.unwrap();
        let listed = app.get(&app.owner, "/api/chats").await;
        let item = find(&listed, "id", text(&chat, "id"));
        assert_eq!(item["unread"], true);
        assert_eq!(item["pendingQuestions"], index + 1);
        assert_eq!(
            app.read(&app.owner, &[chat_read(text(&chat, "id"), &item["readAt"])])
                .await
                .0,
            StatusCode::OK
        );
        let listed = app.get(&app.owner, "/api/chats").await;
        let item = find(&listed, "id", text(&chat, "id"));
        assert_eq!(item["unread"], false);
        assert_eq!(item["pendingQuestions"], index + 1);
        assert_eq!(item["status"], "running");
        let member = app.get(&app.member, "/api/chats").await;
        assert_eq!(find(&member, "id", text(&chat, "id"))["unread"], true);
    }
}

#[tokio::test]
async fn completed_delegated_conversations_do_not_create_unread_notifications() {
    let app = App::new().await;
    app.get(&app.owner, "/api/chats").await;
    let parent = app.finished_chat().await;
    let parent = app
        .service
        .conversation_boundary(text(&parent, "id"))
        .await
        .unwrap();
    let child = app.finished_chat().await;
    let mut stored = app
        .service
        .conversation_boundary(text(&child, "id"))
        .await
        .unwrap();
    stored["requestedBy"] =
        json!({ "chatId": parent["id"], "runId": parent["runId"], "requestId": null });
    app.service
        .store
        .save_conversation_boundary(stored)
        .await
        .unwrap();
    let listed = app.get(&app.owner, "/api/chats").await;
    let item = find(&listed, "id", text(&child, "id"));
    assert_eq!(item["status"], "succeeded");
    assert_eq!(item["unread"], false);
    assert_eq!(item["requester"]["chatId"], parent["id"]);
    assert!(item["preview"].is_null());
}
