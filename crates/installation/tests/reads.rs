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

    async fn dismiss(&self, items: Value) -> (StatusCode, Value) {
        self.request(
            &self.owner,
            "POST",
            "/api/dismissals",
            json!({ "items": items }),
        )
        .await
    }

    async fn read(&self, who: &RelayContext, items: &[String]) -> (StatusCode, Value) {
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
        let (run_id, task) = (run.clone(), task.to_owned());
        self.service
            .store
            .transaction(move |db| {
                db.0.execute(
                    "INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?2,'',?3,?4,?5)",
                    rusqlite::params![run_id, task, status.as_str(), now(), record.to_string()],
                )?;
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
        let mut stored = self.service.get("chats", text(&chat, "id")).await.unwrap();
        stored["runId"] = run.into();
        self.service.store.put("chats", stored).await.unwrap();
        chat
    }
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

    let (status, marked) = app.read(&app.owner, &[format!("chat:{chat_id}")]).await;
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
    let mut stored = app.service.get("chats", chat_id).await.unwrap();
    stored["runId"] = run.into();
    app.service.store.put("chats", stored).await.unwrap();
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
        let mut stored = app.service.get("chats", text(&chat, "id")).await.unwrap();
        stored["runId"] = run.into();
        app.service.store.put("chats", stored).await.unwrap();
        let listed = app.get(&app.owner, "/api/chats").await;
        assert_eq!(find(&listed, "id", text(&chat, "id"))["unread"], false);
    }
}

#[tokio::test]
async fn mission_runs_are_unread_until_their_mission_is_opened() {
    let app = App::new().await;
    let task = app
        .service
        .task(
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

    let (status, _) = app.read(&app.owner, &[format!("task:{task_id}")]).await;
    assert_eq!(status, StatusCode::OK);
    let runs = app.get(&app.owner, "/api/tasks/activity").await;
    assert_eq!(find(&runs, "id", &run)["unread"], false);
}

#[tokio::test]
async fn marking_as_read_accepts_only_conversations_and_missions() {
    let app = App::new().await;
    for items in [
        json!([]),
        json!(["agent:00000000-0000-4000-8000-000000000000"]),
        json!(["chat:not-a-uuid"]),
        json!([7]),
        json!([{ "key": format!("chat:{}", id()), "finishedAt": 0 }]),
        json!([{ "key": format!("chat:{}", id()), "finishedAt": now() + 60_000 }]),
        json!([{ "key": format!("chat:{}", id()) }]),
        json!(vec![format!("chat:{}", id()); 201]),
    ] {
        let (status, body) = app
            .request(&app.member, "POST", "/api/reads", json!({ "items": items }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{items}: {body}");
    }
    let (status, body) = app
        .read(
            &app.member,
            &[format!("chat:{}", id()), format!("task:{}", id())],
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
    let mut stored = app.service.get("chats", chat_id).await.unwrap();
    stored["runId"] = run.into();
    app.service.store.put("chats", stored).await.unwrap();

    let read = |at| {
        json!({
            "items": [{ "key": format!("chat:{chat_id}"), "finishedAt": at }],
        })
    };
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
        .task(
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
            json!({
                "items": [{ "key": format!("task:{task_id}"), "finishedAt": old["finishedAt"] }],
            }),
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
                    app.read(&app.owner, &[format!("chat:{chat_id}")]).await;
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

/// Local SQLite measurements, comparing the original overwrite with observed monotonic marks.
#[tokio::test]
#[ignore = "manual storage measurements"]
async fn read_watermark_measurements() {
    use cairn_installation::store::Store;
    use std::time::Instant;

    fn cpu_ms() -> f64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // CLOCK_PROCESS_CPUTIME_ID includes the SQLite actor threads.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
            0
        );
        time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1_000_000.0
    }

    for monotonic in [false, true] {
        let root = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let finished_at = 1_792_000_000_000_i64;
        let keys: Vec<String> = (0..200)
            .map(|n| format!("read:account:chat:{n:08x}-0000-4000-8000-000000000000"))
            .collect();
        let records = keys.clone();
        let cpu_start = cpu_ms();
        let start = Instant::now();
        for _ in 0..100 {
            let keys = keys.clone();
            store
                .transaction(move |db| {
                    for key in keys {
                        let at = if monotonic {
                            db.kv_as::<i64>(&key)?.unwrap_or(0).max(finished_at)
                        } else {
                            finished_at
                        };
                        db.set(&key, &at.into(), None)?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
        let write_ms = start.elapsed().as_secs_f64() * 10.0;
        let write_cpu_ms = (cpu_ms() - cpu_start) / 100.0;
        drop(store);

        // New connections and empty statement caches; OS page cache remains warm.
        let store = Store::open(root.path()).unwrap();
        let start = Instant::now();
        let marks = store.keys_as::<i64>("read:account:").await.unwrap();
        let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(marks.len(), 200);
        let cpu_start = cpu_ms();
        let start = Instant::now();
        for _ in 0..100 {
            assert_eq!(
                store.keys_as::<i64>("read:account:").await.unwrap().len(),
                200
            );
        }
        let warm_ms = start.elapsed().as_secs_f64() * 10.0;
        let read_cpu_ms = (cpu_ms() - cpu_start) / 100.0;
        let encoded_bytes: usize = records
            .iter()
            .map(|key| key.len() + finished_at.to_string().len())
            .sum();
        println!(
            "monotonic={monotonic} marks=200 encoded_bytes={encoded_bytes} cold_ms={cold_ms:.3} warm_ms={warm_ms:.3} write_ms={write_ms:.3} read_cpu_ms={read_cpu_ms:.3} write_cpu_ms={write_cpu_ms:.3}"
        );
    }
}

#[tokio::test]
#[ignore = "manual storage measurements"]
async fn dismissal_storage_measurements() {
    use cairn_installation::store::Store;
    use std::time::Instant;

    #[derive(serde::Serialize, serde::Deserialize, Clone)]
    #[serde(rename_all = "camelCase")]
    struct Dismissal {
        finished_at: i64,
        until: Option<i64>,
    }

    fn cpu_ms() -> f64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
            0
        );
        time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1_000_000.0
    }

    async fn read(store: &Store, dismissals: bool) {
        store
            .read(move |db| {
                assert!(db.kv_as::<i64>("read-since:account")?.is_some());
                assert_eq!(db.keys_as::<i64>("read:account:")?.len(), 200);
                if dismissals {
                    assert_eq!(db.keys_as::<Dismissal>("dismissed:account:")?.len(), 200);
                }
                Ok(())
            })
            .await
            .unwrap();
    }

    for dismissals in [false, true] {
        let root = TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let finished_at = 1_792_000_000_000_i64;
        let marks: Vec<String> = (0..200)
            .map(|n| format!("read:account:task:{n:08x}-0000-4000-8000-000000000000"))
            .collect();
        let outcomes: Vec<(String, Dismissal)> = (0..200)
            .map(|n| {
                (
                    format!("dismissed:account:task:{n:08x}-0000-4000-8000-000000000000"),
                    Dismissal {
                        finished_at,
                        until: (n % 2 == 0).then_some(finished_at + 86_400_000),
                    },
                )
            })
            .collect();
        let seed_marks = marks.clone();
        store
            .transaction(move |db| {
                db.set("read-since:account", &finished_at.into(), None)?;
                for key in seed_marks {
                    db.set(&key, &finished_at.into(), None)?;
                }
                Ok(())
            })
            .await
            .unwrap();

        let cpu_start = cpu_ms();
        let start = Instant::now();
        for _ in 0..100 {
            let marks = marks.clone();
            let outcomes = outcomes.clone();
            store
                .transaction(move |db| {
                    if dismissals {
                        for (key, value) in outcomes {
                            db.set_as(&key, &value, None)?;
                        }
                    } else {
                        for key in marks {
                            let at = db.kv_as::<i64>(&key)?.unwrap_or(0).max(finished_at);
                            db.set(&key, &at.into(), None)?;
                        }
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
        let write_ms = start.elapsed().as_secs_f64() * 10.0;
        let write_cpu_ms = (cpu_ms() - cpu_start) / 100.0;
        drop(store);

        // Empty SQLite connection/statement caches; OS page cache stays warm.
        let store = Store::open(root.path()).unwrap();
        let start = Instant::now();
        read(&store, dismissals).await;
        let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
        let cpu_start = cpu_ms();
        let start = Instant::now();
        for _ in 0..100 {
            read(&store, dismissals).await;
        }
        let warm_ms = start.elapsed().as_secs_f64() * 10.0;
        let read_cpu_ms = (cpu_ms() - cpu_start) / 100.0;
        let watermark_bytes = "read-since:account".len() + finished_at.to_string().len();
        let mark_bytes: usize = marks
            .iter()
            .map(|key| key.len() + finished_at.to_string().len())
            .sum();
        let dismissal_bytes: usize = if dismissals {
            outcomes
                .iter()
                .map(|(key, value)| key.len() + serde_json::to_vec(value).unwrap().len())
                .sum()
        } else {
            0
        };
        let encoded_bytes = watermark_bytes + mark_bytes + dismissal_bytes;
        println!(
            "dismissals={dismissals} marks=200 outcomes={} encoded_bytes={encoded_bytes} cold_ms={cold_ms:.3} warm_ms={warm_ms:.3} write_ms={write_ms:.3} read_cpu_ms={read_cpu_ms:.3} write_cpu_ms={write_cpu_ms:.3}",
            if dismissals { 200 } else { 0 }
        );
    }
}

#[tokio::test]
async fn blocked_missions_stay_set_aside_for_each_account_until_they_finish_again() {
    let app = App::new().await;
    let task = app
        .service
        .task(
            json!({ "name": "Device proof", "prompt": "Prove it", "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let task_id = text(&task, "id");
    let key = format!("task:{task_id}");
    let blocked =
        |status: &str| json!({ "outcome": { "status": status, "reason": "CI is the parent's." } });
    let run = app.finished_run(task_id, "mcp", RunStatus::Succeeded).await;
    let stored = app
        .service
        .store
        .patch_run(&run, blocked("blocked"))
        .await
        .unwrap();
    let finished_at = stored["finishedAt"].as_i64().unwrap();
    assert!(app.dismissal(&app.owner, &run).await.is_null());

    let (status, body) = app
        .dismiss(json!([{ "key": key, "finishedAt": finished_at }]))
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
        .patch_run(&run, blocked("needs_input"))
        .await
        .unwrap();
    assert!(app.dismissal(&app.owner, &run).await.is_null());
    let until = now() + 60_000;
    app.dismiss(json!([{ "key": key, "finishedAt": finished_at, "until": until }]))
        .await;
    assert_eq!(
        app.dismissal(&app.owner, &run).await,
        json!({ "until": until })
    );

    let (status, _) = app.dismiss(json!([{ "key": key, "restore": true }])).await;
    assert_eq!(status, StatusCode::OK);
    assert!(app.dismissal(&app.owner, &run).await.is_null());

    // A later finish asks again.
    app.dismiss(json!([{ "key": key, "finishedAt": finished_at }]))
        .await;
    let next = app.finished_run(task_id, "mcp", RunStatus::Succeeded).await;
    app.service
        .store
        .patch_run(&next, blocked("blocked"))
        .await
        .unwrap();
    let runs = app.get(&app.owner, "/api/tasks/activity").await;
    assert!(find(&runs, "id", &next)["dismissal"].is_null());

    for items in [
        json!([]),
        json!([{ "key": format!("chat:{}", id()), "finishedAt": finished_at }]),
        json!([{ "key": key }]),
        json!([{ "key": key, "finishedAt": finished_at, "until": now() - 1 }]),
        json!([{ "key": key, "finishedAt": finished_at, "until": now() + 31 * 24 * 60 * 60 * 1000 }]),
    ] {
        let (status, body) = app.dismiss(items.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{items}: {body}");
    }
}
