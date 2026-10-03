mod common;

use common::Fixture;
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn login(app: &Fixture, email: &str) -> (String, Value) {
    let challenge: Value = app
        .post("/api/account/email-code", json!({ "email": email }))
        .await
        .json()
        .await
        .unwrap();
    let code = app.mail.0.lock().unwrap().last().unwrap().1.clone();
    let response = app
        .post(
            "/api/account/verify",
            json!({ "challenge": challenge["challenge"], "code": code }),
        )
        .await;
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (cookie, response.json().await.unwrap())
}

#[tokio::test]
async fn owner_claims_an_installation_with_a_single_use_code() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "owner@example.test").await;
    let response = app
        .client
        .post(format!("{}/api/installations/claim-code", app.url))
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let claim: Value = response.json().await.unwrap();
    assert_eq!(claim["expiresIn"], 600);
    let claim_body = json!({
        "code": claim["code"],
        "name": "My installation",
        "protocol": 1,
    });
    let response = app
        .client
        .post(format!("{}/api/relay/claim", app.url))
        .json(&claim_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let identity: Value = response.json().await.unwrap();
    assert!(identity["token"].as_str().unwrap().len() >= 32);
    assert_eq!(
        app.client
            .post(format!("{}/api/relay/claim", app.url))
            .json(&claim_body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let session: Value = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        session["installations"][0]["id"],
        identity["installationId"]
    );
    assert_eq!(session["installations"][0]["name"], "My installation");
    app.close().await;
}

#[tokio::test]
async fn owner_uses_the_real_installation_api_over_an_outbound_relay() {
    use leo_agent_manager::{config::Config, service::Service};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "relay-owner@example.test").await;
    let claim: Value = app
        .client
        .post(format!("{}/api/installations/claim-code", app.url))
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let config: Config = serde_json::from_value(json!({
        "dataDir": root.path().join("data"),
        "home": root.path().join("home"),
        "workspaceRoots": [root.path()],
        "publicUrl": "http://localhost:4310",
        "host": "127.0.0.1",
        "port": 0,
        "setupToken": "fixture",
        "codexBin": "codex",
        "claudeBin": "claude",
        "ghBin": "gh",
        "concurrency": 1,
        "logger": false,
        "workerEnabled": false,
        "runnerUrl": "",
    }))
    .unwrap();
    let installation = Service::new(config).await.unwrap();
    let router = leo_agent_manager::http::router(installation.clone())
        .await
        .unwrap();
    let identity_dir = root.path().join("relay");
    leo_agent_manager::relay::claim(
        &app.url,
        &identity_dir,
        claim["code"].as_str().unwrap(),
        "Real installation",
    )
    .await
    .unwrap();
    let session: Value = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = session["installations"][0]["id"].as_str().unwrap();
    let stop = CancellationToken::new();
    let connector = tokio::spawn(leo_agent_manager::relay::connect(
        identity_dir,
        router,
        stop.clone(),
    ));
    let base = format!("{}/api/installations/{id}/api", app.url);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = app
                .client
                .get(format!("{base}/chats"))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                assert_eq!(response.json::<Value>().await.unwrap(), json!([]));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("installation must become accessible through the relay");
    let request = |path: String, body: Value| {
        app.client
            .post(path)
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&body)
    };
    let response = request(format!("{base}/chats"), json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chat: Value = response.json().await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let response = request(
        format!("{base}/chats/{chat_id}/messages"),
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "text": "Relayed message",
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let detail: Value = app
        .client
        .get(format!("{base}/chats/{chat_id}"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["messages"][0]["text"], "Relayed message");
    let (other_cookie, _) = login(&app, "other@example.test").await;
    assert_eq!(
        app.client
            .get(format!("{base}/chats"))
            .header("cookie", other_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    stop.cancel();
    connector.await.unwrap().unwrap();
    installation.shutdown.cancel();
    installation.avatars.close().await;
    app.close().await;
}
