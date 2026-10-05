mod common;

use common::{RelayedInstallation, login};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn device_claim_reclaims_a_detached_installation_and_preserves_its_data() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let dir = relay
        .installation
        .config
        .data_dir
        .join("installation-relay");
    let path = dir.join("identity.json");
    let old: Value = serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    let id = old["installationId"].as_str().unwrap();
    let body = json!({
        "name": "Recovered installation",
        "protocol": 1,
        "identity": old,
    });
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
    let blind_approval = relay
        .app
        .client
        .post(format!("{}/api/installations/device-claim", relay.app.url))
        .header("origin", &relay.app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .json(&json!({ "code": device["userCode"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        blind_approval.status(),
        StatusCode::BAD_REQUEST,
        "entering a code alone must never approve it"
    );
    let reviewed = relay
        .app
        .client
        .post(format!(
            "{}/api/installations/device-claim/preview",
            relay.app.url
        ))
        .header("origin", &relay.app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .json(&json!({ "code": device["userCode"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(reviewed.status(), StatusCode::OK);
    let reviewed: Value = reviewed.json().await.unwrap();
    assert_eq!(reviewed["name"], "Real installation");
    assert_eq!(reviewed["fingerprint"], device["fingerprint"]);
    assert_eq!(
        poll().await.status(),
        StatusCode::ACCEPTED,
        "review alone must not approve the installation",
    );
    let (foreign_cookie, foreign_session) =
        login(&relay.app, "foreign-reviewer@example.test").await;
    let foreign_approval = relay
        .app
        .client
        .post(format!("{}/api/installations/device-claim", relay.app.url))
        .header("origin", &relay.app.url)
        .header("cookie", foreign_cookie)
        .header("x-csrf-token", foreign_session["csrf"].as_str().unwrap())
        .json(&json!({
            "code": device["userCode"],
            "confirmation": reviewed["confirmation"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        foreign_approval.status(),
        StatusCode::NOT_FOUND,
        "confirmation is bound to the reviewing account"
    );
    assert_eq!(poll().await.status(), StatusCode::ACCEPTED);
    let approve = |csrf: &str| {
        relay
            .app
            .client
            .post(format!("{}/api/installations/device-claim", relay.app.url))
            .header("origin", &relay.app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({
                "code": device["userCode"],
                "confirmation": reviewed["confirmation"],
            }))
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
    assert!(
        claimed["token"] != old["token"],
        "reclamation must rotate the credential"
    );
    assert_eq!(poll().await.status(), StatusCode::UNAUTHORIZED);
    // A successful response can be lost before the private file is replaced.
    // After detaching, the machine must still recover with the file it owns.
    assert_eq!(
        relay
            .app
            .client
            .post(format!("{}/api/installations/{id}/detach", relay.app.url))
            .header("origin", &relay.app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let recovery = start().await;
    assert_eq!(
        recovery.status(),
        StatusCode::CREATED,
        "lost identity delivery must remain recoverable"
    );
    let recovery: Value = recovery.json().await.unwrap();
    let reviewed: Value = relay
        .app
        .client
        .post(format!(
            "{}/api/installations/device-claim/preview",
            relay.app.url
        ))
        .header("origin", &relay.app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .json(&json!({ "code": recovery["userCode"] }))
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
            .post(format!("{}/api/installations/device-claim", relay.app.url))
            .header("origin", &relay.app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({
                "code": recovery["userCode"],
                "confirmation": reviewed["confirmation"],
            }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let response = relay
        .app
        .post(
            "/api/relay/device-claim/poll",
            json!({ "deviceCode": recovery["deviceCode"] }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let claimed: Value = response.json().await.unwrap();
    assert_eq!(claimed["installationId"], old["installationId"]);
    // The recovery proof must never restore a revoked tunnel credential.
    tokio::time::timeout(
        Duration::from_secs(5),
        leo_agent_manager::relay::connect(
            dir.clone(),
            leo_agent_manager::http::router(relay.installation.clone())
                .await
                .unwrap(),
            relay.installation.clone(),
            relay.stop.clone(),
        ),
    )
    .await
    .expect("old proof must be refused by the relay even after recovery")
    .unwrap();
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
        relay.installation.clone(),
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
