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
