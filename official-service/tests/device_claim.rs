mod common;

use common::{RelayedInstallation, login};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn device_claim_reclaims_a_detached_installation_and_preserves_its_data() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let dir = relay.root.path().join("relay");
    let path = dir.join("identity.json");
    let old: Value = serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    let id = old["installationId"].as_str().unwrap();
    let body = json!({"name": "Recovered installation", "protocol": 1, "identity": old});
    let start = || {
        relay
            .app
            .post("/api/relay/device-claim/start", body.clone())
    };
    assert_eq!(
        start().await.status(),
        StatusCode::CONFLICT,
        "an owned installation cannot change owners"
    );
    let chat: Value = relay
        .app
        .client
        .post(format!("{}/chats", relay.base))
        .header("origin", &relay.app.url)
        .header("cookie", &relay.cookie)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        relay
            .app
            .client
            .post(format!("{}/api/installations/{id}/detach", relay.app.url))
            .header("origin", &relay.app.url)
            .header("cookie", &relay.cookie)
            .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    (&mut relay.connector).await.unwrap().unwrap();
    let started = start().await;
    assert_eq!(started.status(), StatusCode::CREATED);
    let device: Value = started.json().await.unwrap();
    let poll = || {
        relay.app.post(
            "/api/relay/device-claim/poll",
            json!({"deviceCode": device["deviceCode"]}),
        )
    };
    assert_eq!(poll().await.status(), StatusCode::ACCEPTED);
    let (cookie, session) = login(&relay.app, "next-owner@example.test").await;
    let approve = |csrf: &str| {
        relay
            .app
            .client
            .post(format!("{}/api/installations/device-claim", relay.app.url))
            .header("origin", &relay.app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({"code": device["userCode"]}))
    };
    assert_eq!(
        approve("").send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        approve(session["csrf"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        approve(session["csrf"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = poll().await;
    assert_eq!(response.status(), StatusCode::OK);
    let claimed: Value = response.json().await.unwrap();
    assert_eq!(claimed["installationId"], old["installationId"]);
    assert_ne!(claimed["token"], old["token"]);
    assert_eq!(poll().await.status(), StatusCode::UNAUTHORIZED);
    // Machine adapter setup: the CLI's protected file replacement is covered
    // through the real leo claim process in Playwright.
    let mut identity = old;
    identity["token"] = claimed["token"].clone();
    tokio::fs::write(&path, serde_json::to_vec(&identity).unwrap())
        .await
        .unwrap();
    relay.cookie = cookie;
    relay.session = session;
    relay.connector = tokio::spawn(leo_agent_manager::relay::connect(
        dir,
        leo_agent_manager::http::router(relay.installation.clone())
            .await
            .unwrap(),
        relay.stop.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = relay.get("/chats").send().await.unwrap();
            if response.status() == StatusCode::OK {
                assert_eq!(response.json::<Value>().await.unwrap()[0]["id"], chat["id"]);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    relay.close().await;
}
