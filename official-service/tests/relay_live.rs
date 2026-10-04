mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::Value;
use std::time::Duration;

struct Stream {
    response: reqwest::Response,
    pending: String,
}

impl Stream {
    async fn batch(&mut self) -> (i64, Value) {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Some(end) = self.pending.find("\n\n") {
                    let frame: String = self.pending.drain(..end + 2).collect();
                    if !frame.lines().any(|line| line == "event: batch") {
                        continue;
                    }
                    let field = |prefix: &str| {
                        frame
                            .lines()
                            .find_map(|line| line.strip_prefix(prefix))
                            .unwrap()
                    };
                    return (
                        field("id: ").parse().unwrap(),
                        serde_json::from_str(field("data: ")).unwrap(),
                    );
                }
                let chunk = self
                    .response
                    .chunk()
                    .await
                    .unwrap()
                    .expect("live stream ended");
                self.pending.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("a relayed batch must arrive promptly")
    }
}

#[tokio::test]
async fn conversation_updates_arrive_through_the_real_relay() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let response = relay.get("/chats/stream").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-security-policy"], "sandbox");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let mut stream = Stream {
        response,
        pending: String::new(),
    };
    assert_eq!(
        stream.batch().await.1["state"]["chats"],
        serde_json::json!([])
    );

    let response = relay
        .app
        .client
        .post(format!("{}/chats", relay.base))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chat: Value = response.json().await.unwrap();
    assert_eq!(
        stream.batch().await.1["state"]["chats"][0]["id"],
        chat["id"]
    );
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn relay_disconnect_replays_activity_and_does_not_stop_the_run() {
    use leo_agent_manager::config::MAIN_AGENT_ID;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let task = relay.installation.task(json!({
        "name": "Relay recovery", "prompt": "Continue independently", "agentId": MAIN_AGENT_ID,
    }), None).await.unwrap();
    let run = relay
        .installation
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let run_id = run["id"].as_str().unwrap().to_owned();
    let active = run_id.clone();
    relay
        .installation
        .store
        .transaction(move |db| {
            db.patch_run(&active, &json!({ "status": "running" }))?;
            db.event(&active, "output", "before disconnect", None)?;
            Ok(())
        })
        .await
        .unwrap();
    let path = format!("/runs/{run_id}/stream");
    let mut stream = Stream {
        response: relay.get(&path).send().await.unwrap(),
        pending: String::new(),
    };
    let (cursor, batch) = stream.batch().await;
    let history = batch["history"].as_str().unwrap();
    assert_eq!(batch["state"]["run"]["status"], "running");
    relay.stop.cancel();
    (&mut relay.connector).await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Ok(Some(_)) = stream.response.chunk().await {}
    })
    .await
    .expect("lost tunnels must close already-open streams");
    assert!(!relay.installation.shutdown.is_cancelled());

    let active = run_id.clone();
    relay
        .installation
        .store
        .transaction(move |db| {
            db.event(&active, "output", "during disconnect", None)?;
            Ok(())
        })
        .await
        .unwrap();
    relay.stop = CancellationToken::new();
    relay.connector = tokio::spawn(leo_agent_manager::relay::connect(
        relay.root.path().join("relay"),
        leo_agent_manager::http::router(relay.installation.clone())
            .await
            .unwrap(),
        relay.stop.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::OK {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let response = relay
        .get(&format!("{path}?history={history}"))
        .header("last-event-id", cursor.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut resumed = Stream {
        response,
        pending: String::new(),
    };
    let (next, batch) = resumed.batch().await;
    assert!(next > cursor);
    assert_eq!(batch["reset"], false);
    let events = batch["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["text"], "during disconnect");
    assert!(events[0]["id"].as_i64().unwrap() > cursor);
    assert_eq!(batch["state"]["run"]["status"], "running");
    drop(resumed);
    relay.close().await;
}
