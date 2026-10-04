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
