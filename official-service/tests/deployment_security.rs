mod common;

use common::Fixture;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlx_core::query::query;

#[tokio::test]
async fn recipient_can_use_the_emailed_code_when_someone_else_requested_it_first() {
    let app = Fixture::new().await;
    let attacker: Value = app
        .post(
            "/api/account/email-code",
            json!({ "email": "victim@example.test" }),
        )
        .await
        .json()
        .await
        .unwrap();
    let code = app.mail.0.lock().unwrap()[0].1.clone();
    // The recipient requests their own challenge immediately, during the cooldown.
    let response = app
        .post(
            "/api/account/email-code",
            json!({ "email": " VICTIM@EXAMPLE.TEST " }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let recipient: Value = response.json().await.unwrap();
    assert_ne!(recipient["challenge"], attacker["challenge"]);
    assert!(recipient.get("code").is_none());
    assert_eq!(app.mail.0.lock().unwrap().len(), 1);
    let verified = app
        .post(
            "/api/account/verify",
            json!({ "challenge": recipient["challenge"], "code": code }),
        )
        .await;
    assert_eq!(verified.status(), StatusCode::OK);
    let session: Value = verified.json().await.unwrap();
    assert_eq!(session["account"]["email"], "victim@example.test");
    // Consuming the code also invalidates every other challenge for that code.
    assert_eq!(
        app.post(
            "/api/account/verify",
            json!({ "challenge": attacker["challenge"], "code": code }),
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    app.close().await;
}

#[tokio::test]
async fn an_address_has_hourly_and_daily_delivery_caps_beyond_the_minute_cooldown() {
    let app = Fixture::new().await;
    for delivered in 0..20 {
        // Advance the delivery cooldown and the previous proof's lifetime.
        // Hour/day buckets keep their real persisted counts.
        query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'delivery:%' OR key LIKE 'email:%'")
            .execute(&app.pool).await.unwrap();
        query("UPDATE email_codes SET expires_at = now() - interval '1 second'")
            .execute(&app.pool)
            .await
            .unwrap();
        if delivered > 0 && delivered % 6 == 0 {
            let blocked = app
                .post(
                    "/api/account/email-code",
                    json!({ "email": "budget@example.test" }),
                )
                .await;
            assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(app.mail.0.lock().unwrap().len(), delivered);
            query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email-hour:%' OR key LIKE 'email:%'")
                .execute(&app.pool).await.unwrap();
        }
        assert_eq!(
            app.post(
                "/api/account/email-code",
                json!({ "email": " BUDGET@example.test " })
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
    }
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key NOT LIKE 'email-day:%'")
        .execute(&app.pool).await.unwrap();
    query("UPDATE email_codes SET expires_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        app.post(
            "/api/account/email-code",
            json!({ "email": "budget@example.test" })
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(app.mail.0.lock().unwrap().len(), 20);
    app.close().await;
}

#[tokio::test]
async fn trusted_proxies_use_the_rightmost_untrusted_address_for_each_clients_quota() {
    let app = Fixture::with_network(
        Default::default(),
        5,
        "127.0.0.1,10.0.0.0/8,::1/128".parse().unwrap(),
    )
    .await;
    for index in 0..11 {
        let forwarded = format!("192.0.2.{},198.51.100.1,10.1.2.3,::1", index + 10);
        let response = app
            .client
            .post(format!("{}/api/account/email-code", app.url))
            .header("origin", &app.url)
            .header("x-forwarded-for", forwarded)
            .json(&json!({"email": "invalid"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if index < 10 {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::TOO_MANY_REQUESTS
            }
        );
    }
    // This different client shares the TCP proxy, but not the first client's quota.
    let response = app
        .client
        .post(format!("{}/api/account/email-code", app.url))
        .header("origin", &app.url)
        .header("x-forwarded-for", "198.51.100.2,10.1.2.3,::1")
        .json(&json!({"email": "second@example.test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    app.close().await;
}
