mod common;

use common::Fixture;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlx_core::query::query;

#[tokio::test]
async fn requesting_another_code_does_not_replace_a_pending_code_even_after_the_cooldown() {
    let app = Fixture::new().await;
    let challenge: Value = app.post("/api/account/email-code", json!({ "email": "victim@example.test" }))
        .await.json().await.unwrap();
    let code = app.mail.0.lock().unwrap()[0].1.clone();
    // Advance only the minute cooldown, not the lifetime of the pending proof.
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
        .execute(&app.pool).await.unwrap();
    let response = app.post("/api/account/email-code", json!({ "email": " VICTIM@EXAMPLE.TEST " })).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(app.mail.0.lock().unwrap().len(), 1);
    let verified = app.post("/api/account/verify", json!({ "challenge": challenge["challenge"], "code": code })).await;
    assert_eq!(verified.status(), StatusCode::OK);
    app.close().await;
}
