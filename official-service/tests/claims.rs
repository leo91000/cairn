mod common;

use common::Fixture;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlx_core::query::query;

#[tokio::test]
async fn claim_codes_require_a_session_and_csrf_and_expire_without_claiming() {
    let app = Fixture::new().await;
    let endpoint = format!("{}/api/installations/claim-code", app.url);
    assert_eq!(
        app.post("/api/installations/claim-code", json!({}))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let challenge: Value = app
        .post(
            "/api/account/email-code",
            json!({ "email": "claims@example.test" }),
        )
        .await
        .json()
        .await
        .unwrap();
    let code = app.mail.0.lock().unwrap().last().unwrap().1.clone();
    let response = app
        .post(
            "/api/account/verify",
            json!({
                "challenge": challenge["challenge"],
                "code": code,
            }),
        )
        .await;
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = response.json().await.unwrap();
    assert_eq!(
        app.client
            .post(&endpoint)
            .header("cookie", &cookie)
            .header("origin", &app.url)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.client
            .post(&endpoint)
            .header("cookie", &cookie)
            .header("origin", "https://foreign.test")
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let issue_code = || {
        app.client
            .post(&endpoint)
            .header("cookie", &cookie)
            .header("origin", &app.url)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
    };
    let code: Value = issue_code().send().await.unwrap().json().await.unwrap();
    // Advance expiry, matching the existing account tests' clock fixture.
    query("UPDATE installation_claim_codes SET expires_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let claim = |code: &Value, protocol| {
        app.client
            .post(format!("{}/api/relay/claim", app.url))
            .json(&json!({
                "code": code["code"],
                "name": "Private installation",
                "protocol": protocol,
            }))
    };
    assert_eq!(
        claim(&code, 1).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let code: Value = issue_code().send().await.unwrap().json().await.unwrap();
    assert_eq!(
        claim(&code, 999).send().await.unwrap().status(),
        StatusCode::CONFLICT
    );
    // Incompatible claims leave the code available; racing valid claims have one winner.
    let (first, second) = tokio::join!(claim(&code, 1).send(), claim(&code, 1).send());
    let statuses = [first.unwrap().status(), second.unwrap().status()];
    assert!(statuses.contains(&StatusCode::CREATED));
    assert!(statuses.contains(&StatusCode::UNAUTHORIZED));
    app.close().await;
}

#[tokio::test]
async fn relay_requires_the_installation_credential_even_with_an_identity_header() {
    let app = Fixture::new().await;
    let response = app
        .client
        .get(format!("{}/api/relay/unknown/connect", app.url))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("authorization", "Bearer fixture-invalid")
        .header("x-leo-role", "owner")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    app.close().await;
}
