use crate::common::run_fixture::DbRunFixture;
use cairn_contracts::EventKind;
use common::MissionFixture;

mod common;

use common::relay_fixture::router;

use axum::http::StatusCode;
use cairn_installation::{
    config::{Config, MAIN_AGENT_ID, id},
    error::Error,
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

/// A queued run served over HTTP, with a trusted owner context to stream it.
struct Fixture {
    _root: TempDir,
    service: Arc<Service>,
    url: String,
    token: String,
    run: String,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        common::managed_codex_home(&root.path().join("home"));
        let config = Config {
            ..common::config(root.path())
        };
        let service = Service::new(config).await.unwrap();
        let token = text(&common::relay_fixture::context(&service).await, "value").to_owned();
        let task = service
            .task_fixture(
                json!({ "name": "Live test", "prompt": "Test", "agentId": MAIN_AGENT_ID }),
                None,
            )
            .await
            .unwrap();
        let run = service
            .enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap();
        let (url, server) = serve(service.clone()).await;
        Self {
            _root: root,
            service,
            url,
            token,
            run: text(&run, "id").into(),
            server,
        }
    }

    async fn open(&self, after: i64) -> Stream {
        self.open_path(
            &format!("/api/runs/{}/stream?after={after}", self.run),
            None,
        )
        .await
    }

    async fn open_path(&self, path: &str, last: Option<i64>) -> Stream {
        let mut request = reqwest::Client::new()
            .get(format!("{}{path}", self.url))
            .header("x-test-relay-token", &self.token)
            .header("accept-encoding", "gzip, br");
        if let Some(last) = last {
            request = request.header("Last-Event-ID", last);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        assert!(!response.headers().contains_key("content-encoding"));
        Stream {
            response,
            pending: String::new(),
            received: 0,
        }
    }

    /// Requests one page of the run history, as the owner.
    async fn history(&self, query: &str) -> reqwest::Response {
        reqwest::Client::new()
            .get(format!(
                "{}/api/runs/{}/history?{query}",
                self.url, self.run
            ))
            .header("x-test-relay-token", &self.token)
            .send()
            .await
            .unwrap()
    }

    /// Appends `count` output events to the run in one transaction.
    async fn append(&self, count: usize) {
        let run = self.run.clone();
        self.service
            .store
            .transaction(move |db| {
                for n in 0..count {
                    db.event(&run, &EventKind::Output, &format!("event-{n}"), None)?;
                }
                Ok(())
            })
            .await
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.shutdown.cancel();
        self.server.abort();
    }
}

async fn serve(service: Arc<Service>) -> (String, tokio::task::JoinHandle<()>) {
    common::serve_locally(router(service).await.unwrap()).await
}

/// The ids of a list of events.
fn event_ids(events: &Value) -> Vec<i64> {
    events
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["id"].as_i64().unwrap())
        .collect()
}

/// A server-sent event stream that yields its `batch` frames.
struct Stream {
    response: reqwest::Response,
    pending: String,
    /// Bytes read from the response body so far.
    received: usize,
}

impl Stream {
    async fn batch(&mut self) -> (i64, Value) {
        self.batch_with_timeout(Duration::from_secs(4)).await
    }

    async fn batch_with_timeout(&mut self, timeout: Duration) -> (i64, Value) {
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(end) = self.pending.find("\n\n") {
                    let frame: String = self.pending.drain(..end + 2).collect();
                    if !frame.lines().any(|l| l == "event: batch") {
                        continue;
                    }
                    let id = frame
                        .lines()
                        .find_map(|l| l.strip_prefix("id: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    let data = frame
                        .lines()
                        .find_map(|l| l.strip_prefix("data: "))
                        .unwrap();
                    return (id, serde_json::from_str(data).unwrap());
                }
                let chunk = self
                    .response
                    .chunk()
                    .await
                    .unwrap()
                    .expect("unexpected EOF");
                self.received += chunk.len();
                self.pending.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("stream stalled")
    }

    async fn through(&mut self, end: i64) -> Vec<i64> {
        let mut ids = Vec::new();
        loop {
            let (id, batch) = self.batch().await;
            ids.extend(event_ids(&batch["events"]));
            if id >= end {
                return ids;
            }
        }
    }
}

async fn ids(f: &Fixture) -> Vec<i64> {
    let run = f.run.clone();
    f.service
        .store
        .read(move |db| {
            Ok(db
                .events_fixture(&run, 0, 10000)?
                .iter()
                .map(|e| e["id"].as_i64().unwrap())
                .collect())
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn cached_resume_drains_backlog_without_waiting_between_live_batches() {
    let f = Fixture::new().await;
    f.service
        .store
        .change_run(
            &f.run,
            cairn_installation::runs::RunChanges {
                trigger: Some(("chat").to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut chat = f.service.chat_create(json!({})).await.unwrap();
    chat["runId"] = f.run.clone().into();
    let path = format!("/api/chats/{}/stream", text(&chat, "id"));
    f.service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
    let mut initial = f.open_path(&format!("{path}?window=1"), None).await;
    let (cursor, batch) = initial.batch().await;
    let history = batch["history"].as_str().unwrap();
    drop(initial);

    f.append(24000).await;
    let run = f.run.clone();
    let expected: Vec<i64> = f
        .service
        .store
        .read(move |db| {
            Ok(db
                .events_fixture(&run, cursor, 25000)?
                .iter()
                .map(|event| event["id"].as_i64().unwrap())
                .collect())
        })
        .await
        .unwrap();
    assert_eq!(expected.len(), 24000);

    let started = Instant::now();
    let mut resumed = f
        .open_path(
            &format!("{path}?after={cursor}&history={history}&window=1"),
            None,
        )
        .await;

    let end = *expected.last().unwrap();
    let mut received = Vec::new();
    let mut pages = 0;
    loop {
        let (id, batch) = resumed.batch().await;
        received.extend(event_ids(&batch["events"]));
        pages += 1;
        if id >= end {
            break;
        }
    }
    let elapsed = started.elapsed();
    eprintln!("24,000 cached events replayed in {pages} pages and {elapsed:?}");
    assert_eq!(received.len(), expected.len());
    assert!(
        received == expected,
        "cached replay changed the event sequence"
    );

    // Coalescing every page would take at least 25 ms each, whatever the host load.
    // Real I/O stays below that floor without bounding a loaded test run by a fixed time.
    let coalescing_floor = Duration::from_millis(25) * pages;
    assert!(
        elapsed < coalescing_floor,
        "cached replay waited for live coalescing between backlog pages: {elapsed:?}"
    );
}

#[tokio::test]
async fn following_viewer_resumes_a_long_backlog_on_the_recent_window() {
    let f = Fixture::new().await;
    f.service
        .store
        .change_run(
            &f.run,
            cairn_installation::runs::RunChanges {
                trigger: Some(("chat").to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut chat = f.service.chat_create(json!({})).await.unwrap();
    chat["runId"] = f.run.clone().into();
    let chat_id = text(&chat, "id").to_owned();
    let path = format!("/api/chats/{chat_id}/stream");
    f.service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
    let mut initial = f.open_path(&format!("{path}?window=1"), None).await;
    let (cursor, batch) = initial.batch().await;
    let history = batch["history"].as_str().unwrap().to_owned();
    drop(initial);
    let resume = |after: i64| format!("{path}?after={after}&history={history}&window=1&recent=1");

    // A backlog that fits in one page still replays, keeping the cached history.
    f.append(100).await;
    let mut short = f.open_path(&resume(cursor), None).await;
    let (cursor, page) = short.batch().await;
    assert_eq!(page["reset"], false);
    assert_eq!(page["more"], false);
    assert_eq!(event_ids(&page["events"]).len(), 100);
    drop(short);

    // Small enough to run beside the 24,000-event replay test without contention.
    f.append(2400).await;
    let run = f.run.clone();
    let backlog: Vec<i64> = f
        .service
        .store
        .read(move |db| {
            Ok(db
                .events_fixture(&run, cursor, 2500)?
                .iter()
                .map(|event| event["id"].as_i64().unwrap())
                .collect())
        })
        .await
        .unwrap();
    let latest = *backlog.last().unwrap();

    // A reader of older messages keeps the complete replay, so its position stays valid.
    let started = Instant::now();
    let mut replay = f
        .open_path(
            &format!("{path}?after={cursor}&history={history}&window=1"),
            None,
        )
        .await;
    assert_eq!(replay.through(latest).await, backlog);
    let replayed = (started.elapsed(), replay.received);
    drop(replay);

    let started = Instant::now();
    let mut resumed = f.open_path(&resume(cursor), None).await;
    let (id, window) = resumed.batch().await;
    let recent = (started.elapsed(), resumed.received);
    eprintln!(
        "2,400-event backlog: replay {:?} / {} bytes, recent window {:?} / {} bytes",
        replayed.0, replayed.1, recent.0, recent.1
    );
    assert_eq!(id, latest);
    assert_eq!(window["reset"], true);
    assert_eq!(window["more"], false);
    assert_eq!(window["hasOlder"], true);
    assert_eq!(window["state"]["chat"]["id"], chat_id.as_str());
    let events = event_ids(&window["events"]);
    assert_eq!(events, backlog[backlog.len() - 100..]);
    assert_eq!(window["oldest"], events[0]);

    // Older messages remain available from the window's boundary.
    let older: Value = reqwest::Client::new()
        .get(format!(
            "{}/api/chats/{chat_id}/history?before={}&history={history}",
            f.url, events[0]
        ))
        .header("x-test-relay-token", &f.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let previous = event_ids(&older["events"]);
    assert_eq!(previous, backlog[backlog.len() - 1100..backlog.len() - 100]);

    // The viewer then follows live output without another reset.
    f.append(1).await;
    let (next, batch) = resumed.batch().await;
    assert_eq!(batch["reset"], false);
    assert_eq!(event_ids(&batch["events"]), vec![next]);
}

#[tokio::test]
async fn resuming_past_replaced_text_snapshots_keeps_the_cached_history() {
    let f = Fixture::new().await;
    f.service
        .store
        .change_run(
            &f.run,
            cairn_installation::runs::RunChanges {
                trigger: Some(("chat").to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut chat = f.service.chat_create(json!({})).await.unwrap();
    chat["runId"] = f.run.clone().into();
    let path = format!("/api/chats/{}/stream", text(&chat, "id"));
    f.service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
    let mut initial = f.open_path(&format!("{path}?window=1"), None).await;
    let (cursor, batch) = initial.batch().await;
    let history = batch["history"].as_str().unwrap().to_owned();
    drop(initial);

    // While the reader was away, an answer streamed as 1300 snapshots after a tool call.
    let run = f.run.clone();
    f.service
        .store
        .transaction(move |db| {
            let tool =
                json!({ "item": { "id": "tool", "type": "command_execution", "command": "ls" } });
            db.event(&run, &EventKind::ItemCompleted, "ls", Some(&tool))?;
            let mut answer = String::new();
            for n in 0..=1300 {
                answer.push_str(&format!("word {n} "));
                let kind = if n == 1300 {
                    "item.completed"
                } else {
                    "item.updated"
                };
                let message =
                    json!({ "item": { "id": "answer", "type": "agent_message", "text": answer } });
                db.event(
                    &run,
                    &EventKind::from_adapter(kind),
                    &answer,
                    Some(&message),
                )?;
            }
            db.event(&run, &EventKind::TurnCompleted, "", None)?;
            Ok(())
        })
        .await
        .unwrap();
    let all = ids(&f).await;
    let replaced = all[all.len() - 1302..all.len() - 2].to_vec();

    // Following or not, the catch-up is the tool call, the final answer, the turn's
    // end and any later output: none of the 1300 replaced snapshots.
    for following in ["&recent=1", ""] {
        let expected: Vec<i64> = ids(&f)
            .await
            .into_iter()
            .filter(|id| *id > cursor && !replaced.contains(id))
            .collect();
        let latest = *expected.last().unwrap();
        let mut resumed = f
            .open_path(
                &format!("{path}?after={cursor}&history={history}&window=1{following}"),
                None,
            )
            .await;
        let (id, batch) = resumed.batch().await;
        assert_eq!(id, latest);
        assert_eq!(batch["reset"], false);
        assert_eq!(batch["more"], false);
        assert!(batch.get("oldest").is_none());
        assert_eq!(event_ids(&batch["events"]), expected);
        assert!(
            batch["events"][1]["payload"]["item"]["text"]
                .as_str()
                .unwrap()
                .ends_with("word 1300 ")
        );

        // Live output then continues on the same connection.
        f.append(1).await;
        let (next, update) = resumed.batch().await;
        assert_eq!(update["reset"], false);
        assert_eq!(event_ids(&update["events"]), vec![next]);
    }
}

#[tokio::test]
async fn idle_streams_refresh_lists_after_external_database_writes() {
    async fn refreshed(stream: &mut Stream, chat_id: &str) {
        loop {
            let (_, batch) = stream.batch_with_timeout(Duration::from_secs(20)).await;
            let chats = batch["state"]["chats"].as_array().unwrap();
            if chats
                .iter()
                .any(|chat| chat["id"] == chat_id && chat["title"] == "After maintenance")
            {
                break;
            }
        }
    }

    let f = Fixture::new().await;
    let chat = f.service.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    let mut conversation = f
        .open_path(
            &format!("/api/chats/{chat_id}/stream?window=1&list=1"),
            None,
        )
        .await;
    conversation.batch().await;
    let mut list = f.open_path("/api/chats/stream", None).await;
    list.batch().await;

    // This bypasses the actor and its revision counter, as an external maintenance tool does.
    let revision = f.service.store.revision();
    let external =
        rusqlite::Connection::open(f.service.config.data_dir.join("manager.db")).unwrap();
    external
        .execute(
            "UPDATE conversations SET title=?,revision=? WHERE id=?",
            rusqlite::params![
                "After maintenance",
                cairn_installation::config::id(),
                chat_id
            ],
        )
        .unwrap();
    drop(external);
    assert_eq!(f.service.store.revision(), revision);

    tokio::join!(
        refreshed(&mut conversation, chat_id),
        refreshed(&mut list, chat_id)
    );
    assert_eq!(f.service.store.revision(), revision);
}

#[tokio::test]
async fn a_conversation_stream_carries_the_conversation_list_only_on_request() {
    async fn paused_list(stream: &mut Stream, first: Option<Value>) {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut next = first;
            loop {
                let batch = match next.take() {
                    Some(batch) => batch,
                    None => stream.batch().await.1,
                };
                if batch["state"]["chats"][0]["paused"] == true {
                    break;
                }
            }
        })
        .await
        .expect("the list must refresh without another write");
    }

    let f = Fixture::new().await;
    let chat = f.service.chat_create(json!({})).await.unwrap();
    let path = format!("/api/chats/{}/stream?window=1", text(&chat, "id"));
    let mut conversation = f.open_path(&path, None).await;
    let (_, batch) = conversation.batch().await;
    assert_eq!(batch["state"]["chat"]["id"], chat["id"]);
    assert!(batch["state"].get("chats").is_none());

    // A browser tab reading a conversation follows the list on the same stream.
    let mut carrying = f.open_path(&format!("{path}&list=1"), None).await;
    let (_, batch) = carrying.batch().await;
    assert_eq!(batch["state"]["chats"][0]["id"], chat["id"]);

    let mut list = f.open_path("/api/chats/stream", None).await;
    let (_, batch) = list.batch().await;
    assert_eq!(batch["state"]["chats"][0]["id"], chat["id"]);

    // Both follow changes; the list stream at most every quarter of a second.
    f.service.chat_pause(text(&chat, "id"), true).await.unwrap();
    let (_, batch) = carrying.batch().await;
    assert_eq!(batch["state"]["chat"]["paused"], true);
    // The chat's own state is immediate. The shared list may coalesce this
    // commit, but its deadline must deliver it without another database write.
    tokio::join!(
        paused_list(&mut carrying, Some(batch)),
        paused_list(&mut list, None)
    );
}

#[tokio::test]
async fn multiple_clients_refresh_disconnect_and_catch_up_without_gaps() {
    let f = Fixture::new().await;
    f.append(237).await;
    let expected = ids(&f).await;
    let end = *expected.last().unwrap();
    let (mut a, mut b) = tokio::join!(f.open(0), f.open(0));
    let (a_ids, b_ids) = tokio::join!(a.through(end), b.through(end));
    assert_eq!(a_ids, expected);
    assert_eq!(b_ids, expected);
    drop(b);
    let started = Instant::now();
    f.append(23).await;
    let all = ids(&f).await;
    let new: Vec<_> = all.iter().copied().filter(|id| *id > end).collect();
    assert_eq!(a.through(*all.last().unwrap()).await, new);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "live delivery fell back to polling"
    );
    // Header takes precedence over a stale URL cursor (native EventSource resume).
    let mut resumed = f
        .open_path(&format!("/api/runs/{}/stream?after=0", f.run), Some(end))
        .await;
    assert_eq!(resumed.through(*all.last().unwrap()).await, new);
    let mut refreshed = f.open(0).await;
    assert_eq!(refreshed.through(*all.last().unwrap()).await, all);
    let mut ahead = f.open(i64::MAX).await;
    let (_, batch) = ahead.batch().await;
    assert_eq!(batch["reset"], true);
    assert_eq!(batch["events"][0]["id"], all[0]);
}

#[tokio::test]
async fn writes_during_replay_and_slow_readers_do_not_block_or_lose_events() {
    let f = Fixture::new().await;
    f.append(300).await;
    let mut reader = f.open(0).await;
    let (_, first) = reader.batch().await;
    assert_eq!(first["more"], true);
    let slow = f.open(0).await;
    let run = f.run.clone();
    let start = Instant::now();
    f.service
        .store
        .transaction(move |db| {
            for _ in 0..500 {
                db.event(&run, &EventKind::Output, &"x".repeat(16000), None)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    let all = ids(&f).await;
    let mut received = event_ids(&first["events"]);
    received.extend(reader.through(*all.last().unwrap()).await);
    assert_eq!(received, all);
    drop(slow);
    // Rolled-back events are never observable.
    let run = f.run.clone();
    let result = f
        .service
        .store
        .transaction(move |db| {
            db.event(&run, &EventKind::Output, "must not escape", None)?;
            Err::<(), _>(Error::bad("rollback"))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(ids(&f).await, all);
}

#[tokio::test]
async fn server_restart_replays_committed_history_and_continues_live() {
    let mut f = Fixture::new().await;
    f.append(3).await;
    let mut stream = f.open(0).await;
    let cursor = *ids(&f).await.last().unwrap();
    stream.through(cursor).await;
    f.service.shutdown.cancel();
    f.server.abort();
    drop(stream);
    f.service = Service::new(f.service.config.clone()).await.unwrap();
    let (url, server) = serve(f.service.clone()).await;
    f.url = url;
    f.server = server;
    f.append(5).await;
    let all = ids(&f).await;
    let mut resumed = f.open(cursor).await;
    assert_eq!(
        resumed.through(*all.last().unwrap()).await,
        all.into_iter()
            .filter(|id| *id > cursor)
            .collect::<Vec<_>>()
    );
    f.append(1).await;
    assert_eq!(
        resumed.batch().await.1["events"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn metadata_shutdown_and_invalid_requests() {
    let f = Fixture::new().await;
    let mut stream = f.open(0).await;
    stream.batch().await;
    f.service
        .store
        .change_run(
            &f.run,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                boundary: Some(serde_json::json!({"result": "Done"})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (_, batch) = stream.batch().await;
    assert_eq!(batch["state"]["run"]["result"], "Done");
    f.service.shutdown.cancel();
    let ended = tokio::time::timeout(Duration::from_secs(2), stream.response.chunk())
        .await
        .unwrap();
    assert!(ended.unwrap().is_none());
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{}/api/runs/{}/stream", f.url, f.run))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let token = text(&common::relay_fixture::context(&f.service).await, "value").to_owned();
    for (path, code) in [
        (
            format!("/api/runs/{}/stream?after=-1", f.run),
            StatusCode::BAD_REQUEST,
        ),
        ("/api/runs/not-an-id/stream".into(), StatusCode::BAD_REQUEST),
        (format!("/api/runs/{}/stream", id()), StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            client
                .get(format!("{}{path}", f.url))
                .header("x-test-relay-token", &token)
                .send()
                .await
                .unwrap()
                .status(),
            code
        );
    }
}

#[tokio::test]
async fn chat_state_questions_and_artifacts_update_without_text_events() {
    let f = Fixture::new().await;
    let chat = f.service.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    let mut stream = f
        .open_path(&format!("/api/chats/{chat_id}/stream"), None)
        .await;
    let (_, first) = stream.batch().await;
    assert!(first["state"]["run"].is_null());
    let mut attached = chat.clone();
    attached["runId"] = f.run.clone().into();
    f.service
        .store
        .save_conversation_boundary(attached)
        .await
        .unwrap();
    let (_, batch) = stream.batch().await;
    assert_eq!(batch["state"]["run"]["id"], f.run);
    assert_eq!(batch["reset"], true);
    f.service.chat_pause(chat_id, true).await.unwrap();
    assert_eq!(stream.batch().await.1["state"]["chat"]["paused"], true);
    // Mutations of metadata have no event id of their own and still reach every client.
    f.service
        .store
        .set(
            &format!("chat-question:{chat_id}:question"),
            json!({
                "id": "question",
                "chatId": chat_id,
                "runId": f.run,
                "blocking": false,
                "fields": [],
                "status": "pending",
            }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        stream.batch().await.1["state"]["chat"]["pendingQuestions"],
        1
    );
    f.service
        .store
        .set(
            &format!("artifact:{}:file", f.run),
            json!({ "id": "file", "previewStatus": "pending" }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        stream.batch().await.1["state"]["artifacts"][0]["previewStatus"],
        "pending"
    );
    f.service
        .store
        .set(
            &format!("artifact:{}:file", f.run),
            json!({ "id": "file", "previewStatus": "ready" }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        stream.batch().await.1["state"]["artifacts"][0]["previewStatus"],
        "ready"
    );
}

#[tokio::test]
async fn live_delivery_latency_under_repeated_writes() {
    let f = Fixture::new().await;
    let mut stream = f.open(0).await;
    let initial = *ids(&f).await.last().unwrap();
    stream.through(initial).await;
    let mut samples = Vec::new();
    for _ in 0..25 {
        let start = Instant::now();
        f.append(1).await;
        let (_, batch) = stream.batch().await;
        assert_eq!(batch["events"].as_array().unwrap().len(), 1);
        samples.push(start.elapsed().as_millis());
    }
    samples.sort_unstable();
    println!(
        "SQLite write → HTTP client (25 samples): p50={}ms p95={}ms max={}ms",
        samples[12], samples[23], samples[24]
    );
    assert!(
        samples[23] < 500,
        "delivery must not depend on the former 1.2s polling cadence"
    );
}

#[tokio::test]
async fn wire_deltas_reestablish_baselines_after_reconnect_and_keep_rest_compatible() {
    let f = Fixture::new().await;
    let mut stream = f.open(0).await;
    stream.batch().await;
    let publish = async |content: &str| {
        f.service
            .store
            .event(
                &f.run,
                &EventKind::ItemUpdated,
                content,
                Some(json!({
                    "item": { "id": "message", "type": "agent_message", "text": content },
                })),
            )
            .await
            .unwrap();
    };
    publish("Bonjour 👋").await;
    let (cursor, first) = stream.batch().await;
    assert_eq!(first["events"][0]["payload"]["item"]["text"], "Bonjour 👋");
    publish("Bonjour 👋 café").await;
    let (_, second) = stream.batch().await;
    assert_eq!(second["events"][0]["payload"]["item"]["delta"], " café");
    assert!(second["events"][0]["payload"]["item"]["text"].is_null());
    let mut resumed = f.open(cursor).await;
    assert_eq!(
        resumed.batch().await.1["events"][0]["payload"]["item"]["text"],
        "Bonjour 👋 café"
    );
    publish("Corrected message").await;
    assert_eq!(
        stream.batch().await.1["events"][0]["payload"]["item"]["text"],
        "Corrected message"
    );
    let run = f.run.clone();
    let stored = f
        .service
        .store
        .read(move |db| db.events_fixture(&run, cursor, 10))
        .await
        .unwrap();
    assert_eq!(stored[0]["payload"]["item"]["text"], "Bonjour 👋 café");
}

#[tokio::test]
async fn cache_revision_resumes_valid_history_and_resets_pruned_or_foreign_history() {
    let fixture = Fixture::new().await;
    fixture.append(3).await;
    let mut first = fixture.open(0).await;
    let (cursor, initial) = first.batch().await;
    let history = initial["history"].as_str().unwrap();
    let mut resumed = fixture
        .open_path(
            &format!(
                "/api/runs/{}/stream?after={cursor}&history={history}",
                fixture.run
            ),
            None,
        )
        .await;
    let (_, batch) = resumed.batch().await;
    assert_eq!(batch["reset"], false);
    assert_eq!(batch["events"].as_array().unwrap().len(), 0);
    let mut foreign = fixture
        .open_path(
            &format!(
                "/api/runs/{}/stream?after={cursor}&history=v1:other:1",
                fixture.run
            ),
            None,
        )
        .await;
    let (_, batch) = foreign.batch().await;
    assert_eq!(batch["reset"], true);
    assert!(!batch["events"].as_array().unwrap().is_empty());
    let run = fixture.run.clone();
    fixture
        .service
        .store
        .transaction(move |db| {
            db.0.execute(
                "DELETE FROM events WHERE run_id=? AND id<?",
                rusqlite::params![run, cursor],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut pruned = fixture
        .open_path(
            &format!(
                "/api/runs/{}/stream?after={cursor}&history={history}",
                fixture.run
            ),
            None,
        )
        .await;
    let (_, batch) = pruned.batch().await;
    assert_eq!(batch["reset"], true);
    assert_ne!(batch["history"], initial["history"]);
    assert_eq!(batch["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn recent_window_pages_backwards_without_gaps_and_keeps_live_cursor() {
    let f = Fixture::new().await;
    f.append(2450).await;
    let all = ids(&f).await;
    let mut stream = f
        .open_path(&format!("/api/runs/{}/stream?window=1", f.run), None)
        .await;
    let (cursor, batch) = stream.batch().await;
    assert_eq!(cursor, *all.last().unwrap());
    assert_eq!(batch["more"], false);
    assert_eq!(batch["hasOlder"], true);
    assert!(batch["state"]["chat"].is_null());
    assert_eq!(batch["events"].as_array().unwrap().len(), 100);
    let history = batch["history"].as_str().unwrap();
    let mut before = batch["oldest"].as_i64().unwrap();
    let mut collected = event_ids(&batch["events"]);
    loop {
        let response = f
            .history(&format!("before={before}&history={history}"))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page: Value = response.json().await.unwrap();
        let mut previous = event_ids(&page["events"]);
        assert!(previous.iter().all(|id| *id < before));
        previous.extend(collected);
        collected = previous;
        before = page["oldest"].as_i64().unwrap();
        if page["hasOlder"] == false {
            break;
        }
    }
    assert_eq!(collected, all);
    f.append(1).await;
    let (next, update) = stream.batch().await;
    assert!(next > cursor);
    assert_eq!(update["events"].as_array().unwrap().len(), 1);
    assert!(update.get("oldest").is_none());
    for (query, expected) in [
        ("before=0&history=x", StatusCode::BAD_REQUEST),
        ("before=5&history=v1:foreign:1", StatusCode::CONFLICT),
        ("before=5", StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(f.history(query).await.status(), expected);
    }
    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/runs/{}/history?history={history}",
            f.url, f.run
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn compressed_history_budget_keeps_repetitive_output_and_preserves_trimmed_cursors() {
    use rand::{RngCore, SeedableRng};
    let f = Fixture::new().await;
    let run = f.run.clone();
    f.service
        .store
        .transaction(move |db| {
            for _ in 0..200 {
                db.event(
                    &run,
                    &EventKind::Output,
                    &"tool output 🦊 ".repeat(700),
                    None,
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let before_noise = ids(&f).await;
    let run = f.run.clone();
    let window = f
        .service
        .store
        .read(move |db| db.events_before(&run, i64::MAX))
        .await
        .unwrap();
    assert!(!window.1);
    assert_eq!(window.0.len(), before_noise.len());
    assert!(serde_json::to_vec(&window.0).unwrap().len() > 256 * 1024);

    let run = f.run.clone();
    f.service
        .store
        .transaction(move |db| {
            let mut rng = rand::rngs::StdRng::seed_from_u64(19);
            for _ in 0..24 {
                let mut noise = vec![0; 96 * 1024];
                rng.fill_bytes(&mut noise);
                db.event(
                    &run,
                    &EventKind::Output,
                    "noise",
                    Some(&json!({"noise": hex::encode(noise)})),
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let expected = ids(&f).await;
    let mut before = i64::MAX;
    let mut collected = Vec::new();
    let mut pages = 0;
    loop {
        let run = f.run.clone();
        let (events, more) = f
            .service
            .store
            .read(move |db| db.events_before(&run, before))
            .await
            .unwrap();
        assert!(!events.is_empty());
        let bytes = serde_json::to_vec(&events).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &bytes).unwrap();
        assert!(encoder.finish().unwrap().len() <= 256 * 1024);
        before = events[0].id;
        let mut next: Vec<_> = events.into_iter().map(|event| event.id).collect();
        next.extend(collected);
        collected = next;
        pages += 1;
        if !more {
            break;
        }
    }
    assert!(pages > 2);
    assert_eq!(collected, expected);
}

#[tokio::test]
async fn compressed_history_bounds_decoded_json_and_advances_past_oversized_events() {
    use rand::{RngCore, SeedableRng};

    let f = Fixture::new().await;
    let run = f.run.clone();
    f.service
        .store
        .transaction(move |db| {
            // JSON escaping makes each payload much larger than its source string.
            for _ in 0..5 {
                db.event(
                    &run,
                    &EventKind::Output,
                    "escaped",
                    Some(&json!({
                        "text": "\u{001f}".repeat(300_000),
                    })),
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let expected = ids(&f).await;
    let run = f.run.clone();
    let (events, more) = f
        .service
        .store
        .read(move |db| db.events_before(&run, i64::MAX))
        .await
        .unwrap();
    assert!(more);
    assert_eq!(events.len(), 2);
    assert!(serde_json::to_vec(&events).unwrap().len() <= 4 * 1024 * 1024);
    let before_escaped = events[0].id;

    let run = f.run.clone();
    f.service
        .store
        .transaction(move |db| {
            // A single event may exceed either ceiling, but must remain readable.
            db.event(
                &run,
                &EventKind::Output,
                "decoded oversized",
                Some(&json!({
                    "text": "q".repeat(5 * 1024 * 1024),
                })),
            )?;
            let mut noise = vec![0; 512 * 1024];
            rand::rngs::StdRng::seed_from_u64(29).fill_bytes(&mut noise);
            db.event(
                &run,
                &EventKind::Output,
                "gzip oversized",
                Some(&json!({
                    "noise": hex::encode(noise),
                })),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut before = i64::MAX;
    for text in ["gzip oversized", "decoded oversized"] {
        let run = f.run.clone();
        let (events, more) = f
            .service
            .store
            .read(move |db| db.events_before(&run, before))
            .await
            .unwrap();
        assert!(more);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, text);
        assert!(events[0].id < before);
        before = events[0].id;
    }
    let mut collected = Vec::new();
    loop {
        let run = f.run.clone();
        let (events, more) = f
            .service
            .store
            .read(move |db| db.events_before(&run, before))
            .await
            .unwrap();
        assert!(!events.is_empty());
        assert!(events[0].id < before);
        before = events[0].id;
        let mut page: Vec<_> = events.into_iter().map(|event| event.id).collect();
        page.extend(collected);
        collected = page;
        if !more {
            break;
        }
    }
    assert_eq!(collected, expected);
    assert!(before < before_escaped);
}

#[tokio::test]
async fn paginated_chat_metadata_keeps_pending_messages_without_replaying_delivered_text() {
    let f = Fixture::new().await;
    let mut chat = f.service.chat_create(json!({})).await.unwrap();
    chat["runId"] = f.run.clone().into();
    let id = text(&chat, "id").to_owned();
    f.service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
    let chat_id = id.clone();
    f.service
        .store
        .transaction(move |db| {
            for status in ["delivered", "queued"] {
                db.put_message(&json!({
                    "id": status,
                    "chatId": chat_id,
                    "createdAt": 1,
                    "status": status,
                    "text": "A message",
                }))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut modern = f
        .open_path(&format!("/api/chats/{id}/stream?window=1"), None)
        .await;
    let (_, batch) = modern.batch().await;
    assert_eq!(
        batch["state"]["chat"]["messages"].as_array().unwrap().len(),
        1
    );
    assert_eq!(batch["state"]["chat"]["messages"][0]["status"], "queued");
    let mut legacy = f.open_path(&format!("/api/chats/{id}/stream"), None).await;
    let (_, batch) = legacy.batch().await;
    assert_eq!(
        batch["state"]["chat"]["messages"].as_array().unwrap().len(),
        2
    );
}

#[tokio::test]
async fn profile_long_answer_delivery_with_small_wire_deltas() {
    let f = Fixture::new().await;
    let mut stream = f
        .open_path(&format!("/api/runs/{}/stream?window=1", f.run), None)
        .await;
    stream.batch().await;
    for length in [2000, 20000, 100000] {
        let mut content = "a".repeat(length);
        let publish = async |value: &str| {
            f.service
                .store
                .event(
                    &f.run,
                    &EventKind::ItemUpdated,
                    value,
                    Some(json!({
                        "item": { "id": "profile", "type": "agent_message", "text": value },
                    })),
                )
                .await
                .unwrap();
        };
        publish(&content).await;
        stream.batch().await;
        let mut times = Vec::new();
        let mut bytes = Vec::new();
        for _ in 0..18 {
            let suffix = " more text".repeat(20);
            content.push_str(&suffix);
            let start = Instant::now();
            publish(&content).await;
            let (_, batch) = stream.batch().await;
            times.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(batch["events"][0]["payload"]["item"]["delta"], suffix);
            bytes.push(serde_json::to_vec(&batch).unwrap().len());
        }
        times.sort_by(f64::total_cmp);
        let profile = json!({
            "characters": length,
            "samples": times.len(),
            "p50_ms": times[9],
            "p95_ms": times[17],
            "max_batch_bytes": bytes.into_iter().max(),
        });
        println!("STREAM_SERVER_PROFILE {profile}");
    }
}

#[tokio::test]
async fn backwards_pages_skip_superseded_long_answers_but_keep_reused_ids_in_older_turns() {
    let f = Fixture::new().await;
    let run = f.run.clone();
    f.service
        .store
        .transaction(move |db| {
            db.event(&run, &EventKind::TurnStarted, "", None)?;
            db.event(
                &run,
                &EventKind::ItemCompleted,
                "previous turn",
                Some(&json!({
                    "item": { "id": "m", "type": "agent_message", "text": "previous turn" },
                })),
            )?;
            db.event(&run, &EventKind::TurnCompleted, "", None)?;
            db.event(&run, &EventKind::TurnStarted, "", None)?;
            db.event(&run, &EventKind::ChatUser, "Older question", None)?;
            for n in 0..80 {
                let text = format!("{}-{n}", "x".repeat(300_000));
                db.event(
                    &run,
                    &EventKind::ItemUpdated,
                    &text,
                    Some(&json!({
                        "item": { "id": "m", "type": "agent_message", "text": text },
                    })),
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut stream = f
        .open_path(&format!("/api/runs/{}/stream?window=1", f.run), None)
        .await;
    let (_, first) = stream.batch().await;
    assert_eq!(first["events"].as_array().unwrap().len(), 1);
    assert!(
        first["events"][0]["payload"]["item"]["text"]
            .as_str()
            .unwrap()
            .ends_with("-79")
    );
    assert_eq!(first["hasOlder"], true);
    let before = first["oldest"].as_i64().unwrap();
    let history = first["history"].as_str().unwrap();
    let page: Value = f
        .history(&format!("before={before}&history={history}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(page["hasOlder"], false);
    let events = page["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["text"] == "Older question"));
    let messages: Vec<_> = events
        .iter()
        .filter(|e| e["payload"]["item"]["type"] == "agent_message")
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["payload"]["item"]["text"], "previous turn");
}

/// Process CPU time, including the SQLite actor threads, in milliseconds.
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

/// Percentile of sorted samples, in milliseconds.
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    (sorted[index] * 100.0).round() / 100.0
}

fn summary(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    json!({
        "samples": samples.len(),
        "p50_ms": percentile(&samples, 0.5),
        "p95_ms": percentile(&samples, 0.95),
    })
}

/// Opt-in: `CAIRN_PROFILE_OPENING=1 cargo test --release --test live profile_conversation_opening -- --nocapture`.
/// Seeds a Fil with 80 active conversations and 200 in the trash, one of them a
/// 40-turn conversation with tool calls and streamed answers, then measures what
/// opening, paging and resuming that conversation costs the installation.
#[tokio::test]
async fn profile_conversation_opening() {
    if std::env::var_os("CAIRN_PROFILE_OPENING").is_none() {
        return;
    }
    let f = Fixture::new().await;
    let template = f.service.store.run(&f.run).await.unwrap();
    let mut chats = Vec::new();
    for n in 0..281 {
        chats.push(f.service.chat_create(json!({})).await.unwrap());
        if n % 50 == 0 {
            tokio::task::yield_now().await;
        }
    }
    let heavy = text(&chats[0], "id").to_owned();
    let seeded = chats.clone();
    let heavy_run = f.run.clone();
    f.service
        .store
        .transaction(move |db| {
            let summary = "A finished answer with **Markdown** and a few lines.\n".repeat(60);
            for (n, chat) in seeded.into_iter().enumerate() {
                let run_id = if n == 0 {
                    heavy_run.clone()
                } else {
                    let mut run = template.clone();
                    let id = id();
                    run["id"] = id.clone().into();
                    run["status"] = "succeeded".into();
                    run["trigger"] = "chat".into();
                    run["summary"] = summary.clone().into();
                    run["finishedAt"] = 1.into();
                    db.add_run_fixture(&run, None)?;
                    id
                };
                let mut chat = chat;
                chat["runId"] = run_id.into();
                if n > 80 {
                    chat["lifecycle"] = "trash".into();
                    chat["purgeAt"] = 9_007_199_254_740_991_i64.into();
                }
                db.save_conversation_boundary(&chat)?;
            }
            let output = "test output line\n".repeat(90);
            for turn in 0..40 {
                db.event(&heavy_run, &EventKind::TurnStarted, "", None)?;
                db.event(&heavy_run, &EventKind::ChatUser, &format!("Question {turn}"), None)?;
                for call in 0..25 {
                    let item = format!("tool-{turn}-{call}");
                    let started = json!({
                        "item": { "id": item, "type": "command_execution", "command": "pnpm test", "status": "in_progress" },
                    });
                    db.event(&heavy_run, &EventKind::ItemStarted, "pnpm test", Some(&started))?;
                    let completed = json!({
                        "item": { "id": item, "type": "command_execution", "command": "pnpm test", "aggregated_output": output, "exit_code": 0, "status": "completed" },
                    });
                    db.event(&heavy_run, &EventKind::ItemCompleted, "pnpm test", Some(&completed))?;
                }
                // The last answer streams as many small snapshots, as a long reply does.
                let updates = if turn == 39 { 300 } else { 40 };
                let mut answer = String::new();
                for update in 0..=updates {
                    answer.push_str(&format!("Paragraph {update} of the answer to turn {turn}. "));
                    answer.push_str(&"More detail. ".repeat(6));
                    let kind = if update == updates { "item.completed" } else { "item.updated" };
                    let message = json!({ "item": { "id": format!("answer-{turn}"), "type": "agent_message", "text": answer } });
                    db.event(&heavy_run, &EventKind::from_adapter(kind), &answer, Some(&message))?;
                }
                db.event(&heavy_run, &EventKind::TurnCompleted, "", None)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let all = ids(&f).await;
    let stream_path = format!("/api/chats/{heavy}/stream");

    // Cold opening: the first batch carries the recent window and its metadata. A browser
    // tab asks its conversation stream to carry the list as well (`list=1`); a reference
    // without that parameter carries the list on every conversation stream.
    let mut cold = Vec::new();
    let mut cold_bytes = 0;
    let mut history = String::new();
    let mut oldest = 0;
    for _ in 0..15 {
        let start = Instant::now();
        let mut stream = f
            .open_path(&format!("{stream_path}?after=0&window=1"), None)
            .await;
        let (_, batch) = stream.batch().await;
        cold.push(start.elapsed().as_secs_f64() * 1000.0);
        cold_bytes = stream.received;
        history = batch["history"].as_str().unwrap().to_owned();
        oldest = batch["oldest"].as_i64().unwrap();
    }
    let mut listed = Vec::new();
    let mut listed_bytes = 0;
    for _ in 0..15 {
        let start = Instant::now();
        let mut stream = f
            .open_path(&format!("{stream_path}?after=0&window=1&list=1"), None)
            .await;
        stream.batch().await;
        listed.push(start.elapsed().as_secs_f64() * 1000.0);
        listed_bytes = stream.received;
    }

    // The conversation list on its own.
    let mut list = Vec::new();
    let mut list_bytes = 0;
    for _ in 0..15 {
        let start = Instant::now();
        let mut stream = f.open_path("/api/chats/stream", None).await;
        stream.batch().await;
        list.push(start.elapsed().as_secs_f64() * 1000.0);
        list_bytes = stream.received;
    }

    // Every older page back to the start of the conversation.
    let mut pages = Vec::new();
    let mut before = oldest;
    loop {
        let start = Instant::now();
        let page: Value = reqwest::Client::new()
            .get(format!(
                "{}/api/chats/{heavy}/history?before={before}&history={history}",
                f.url
            ))
            .header("x-test-relay-token", &f.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        pages.push(start.elapsed().as_secs_f64() * 1000.0);
        if page["hasOlder"] != true {
            break;
        }
        before = page["oldest"].as_i64().unwrap();
    }

    // A reader who left before the last turn comes back to the latest output.
    let resume_after = all[all.len() - 355];
    let mut resume = Vec::new();
    let mut resume_batch = Value::Null;
    let mut resume_bytes = 0;
    for _ in 0..15 {
        let start = Instant::now();
        let mut stream = f
            .open_path(
                &format!("{stream_path}?after={resume_after}&history={history}&window=1&recent=1"),
                None,
            )
            .await;
        let (_, batch) = stream.batch().await;
        resume.push(start.elapsed().as_secs_f64() * 1000.0);
        resume_bytes = stream.received;
        resume_batch = batch;
    }

    // Delivery to a following reader while other tabs keep the list and other conversations
    // (each carrying the list, as a browser tab does) open.
    let mut idle = Vec::new();
    for chat in chats.iter().skip(1).take(6) {
        idle.push(
            f.open_path(
                &format!(
                    "/api/chats/{}/stream?after=0&window=1&list=1",
                    text(chat, "id")
                ),
                None,
            )
            .await,
        );
        idle.push(f.open_path("/api/chats/stream", None).await);
    }
    for stream in &mut idle {
        stream.batch().await;
    }
    let mut follower = f
        .open_path(&format!("{stream_path}?after=0&window=1"), None)
        .await;
    follower.batch().await;
    let cpu = cpu_ms();
    let mut delivery = Vec::new();
    for n in 0..60 {
        let start = Instant::now();
        let run = f.run.clone();
        f.service
            .store
            .transaction(move |db| db.event(&run, &EventKind::Output, &format!("live {n}"), None))
            .await
            .unwrap();
        let (_, batch) = follower.batch().await;
        assert!(!batch["events"].as_array().unwrap().is_empty());
        delivery.push(start.elapsed().as_secs_f64() * 1000.0);
    }

    let delivery_cpu = cpu_ms() - cpu;
    drop(follower);
    let mut carrying = f
        .open_path(&format!("{stream_path}?after=0&window=1&list=1"), None)
        .await;
    carrying.batch().await;
    let cpu = cpu_ms();
    let mut carrying_delivery = Vec::new();
    for n in 0..60 {
        let start = Instant::now();
        let run = f.run.clone();
        f.service
            .store
            .transaction(move |db| {
                db.event(&run, &EventKind::Output, &format!("carrying {n}"), None)
            })
            .await
            .unwrap();
        // A list-only refresh may arrive before the newly appended event.
        loop {
            let (_, batch) = carrying.batch().await;
            if !batch["events"].as_array().unwrap().is_empty() {
                break;
            }
        }
        carrying_delivery.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let profile = json!({
        "events": all.len(),
        "cold_open": summary(cold),
        "cold_open_first_batch_bytes": cold_bytes,
        "cold_open_with_list": summary(listed),
        "cold_open_with_list_first_batch_bytes": listed_bytes,
        "list_open": summary(list),
        "list_first_batch_bytes": list_bytes,
        "history_pages": summary(pages),
        "resume_after_streamed_answer": summary(resume),
        "resume_reset": resume_batch["reset"],
        "resume_events": resume_batch["events"].as_array().unwrap().len(),
        "resume_first_batch_bytes": resume_bytes,
        "delivery_with_12_idle_streams": summary(delivery),
        "delivery_process_cpu_ms": delivery_cpu.round(),
        "delivery_with_list_and_12_idle_streams": summary(carrying_delivery),
        "carrying_delivery_process_cpu_ms": (cpu_ms() - cpu).round(),
    });
    println!("CONVERSATION_OPEN_PROFILE {profile}");
}

#[tokio::test]
async fn typed_history_get_keeps_native_history_readable_to_members_after_agent_deletion() {
    use common::run_fixture::RunFixture;
    let fixture = Fixture::new().await;
    let missing_agent = id();
    fixture
        .service
        .store
        .change_run_fixture(
            &fixture.run,
            json!({
                "snapshot": {"task": {"agentId": missing_agent}, "agent": {"id": missing_agent, "access": {"backend": "native"}}},
            }),
        )
        .await
        .unwrap();
    let token = common::relay_fixture::member_context(&fixture.service, "history-reader").await;
    let run = fixture.run.clone();
    let first = fixture
        .service
        .store
        .read(move |db| Ok(db.event_page(&run, 0, 1)?[0].id))
        .await
        .unwrap();
    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/runs/{}/history?history=v1:{}:{first}",
            fixture.url, fixture.run, fixture.run
        ))
        .header("x-test-relay-token", text(&token, "value"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let page: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["events"][0]["id"], first);
}

#[tokio::test]
async fn conversation_streams_mask_queued_and_sending_private_answers() {
    use cairn_installation::chats::MessageStatus;
    let f = Fixture::new().await;
    let mut chat = f.service.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id").to_owned();
    chat["runId"] = f.run.clone().into();
    f.service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
    let question_id = "a".repeat(64);
    f.service
        .question_receive(
            &f.run,
            json!({
                "id": question_id, "blocking": false,
                "fields": [{ "id": "secret", "title": "Private credential", "secret": true }],
            }),
        )
        .await
        .unwrap();
    let message_id = id();
    f.service.question_answer(&chat_id, &question_id, json!({
        "id": message_id,
        "answers": { "secret": { "selected": [], "comment": "private-answer-must-never-cross-sse" } },
    })).await.unwrap();

    for status in [MessageStatus::Queued, MessageStatus::Sending] {
        let (stored_chat, stored_message) = (chat_id.clone(), message_id.clone());
        f.service
            .store
            .transaction(move |db| {
                let mut message = db
                    .conversation_message(&stored_chat, &stored_message)?
                    .unwrap();
                assert!(
                    serde_json::to_string(&message.answers)?
                        .contains("private-answer-must-never-cross-sse")
                );
                message.status = status;
                db.save_message(&message)
            })
            .await
            .unwrap();
        for window in [false, true] {
            let path = format!("/api/chats/{chat_id}/stream?window={}", u8::from(window));
            let (_, batch) = f.open_path(&path, None).await.batch().await;
            assert!(
                !batch
                    .to_string()
                    .contains("private-answer-must-never-cross-sse"),
                "{batch}"
            );
            let messages = batch["state"]["chat"]["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0]["text"], "Private answer");
            assert!(messages[0].get("answers").is_none());
        }
    }
}
