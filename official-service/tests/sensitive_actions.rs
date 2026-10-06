mod common;

use common::RelayedInstallation;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx_core::query::query;
use std::time::Duration;

#[tokio::test]
async fn detachment_and_forgetting_require_recent_proof_without_disrupting_live_access() {
    for (method, suffix) in [(Method::POST, "/detach"), (Method::DELETE, "")] {
        let relay = RelayedInstallation::new(axum::Router::new()).await;
        let app = &relay.app;
        let id = relay.session["installations"][0]["id"].as_str().unwrap();
        let route = format!("/api/installations/{id}{suffix}");
        query("UPDATE web_sessions SET authenticated_at = NULL")
            .execute(&app.pool)
            .await
            .unwrap();
        let mut stream = relay.get("/chats/stream").send().await.unwrap();
        stream.chunk().await.unwrap().unwrap();

        let rejected = app
            .authenticated(&relay.cookie, &relay.session, method.clone(), &route)
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN, "{route}");
        assert_eq!(
            relay.get("/chats").send().await.unwrap().status(),
            StatusCode::OK
        );

        query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
            .execute(&app.pool)
            .await
            .unwrap();
        let challenge: Value = app
            .post(
                "/api/account/email-code",
                json!({ "email": "relay-owner@example.test" }),
            )
            .await
            .json()
            .await
            .unwrap();
        let code = app.mail.0.lock().unwrap().last().unwrap().1.clone();
        let confirmed = app
            .authenticated(
                &relay.cookie,
                &relay.session,
                Method::POST,
                "/api/account/reauth/email",
            )
            .json(&json!({ "challenge": challenge["challenge"], "code": code }))
            .send()
            .await
            .unwrap();
        assert_eq!(confirmed.status(), StatusCode::NO_CONTENT);
        let removed = app
            .authenticated(&relay.cookie, &relay.session, method, &route)
            .send()
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Ok(Some(_)) = stream.chunk().await {}
        })
        .await
        .expect("confirmed removal must close existing streams");
        let session: Value = app
            .authenticated(
                &relay.cookie,
                &relay.session,
                Method::GET,
                "/api/account/session",
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(session["authenticated"], true);
        assert_eq!(session["installations"], json!([]));
        drop(stream);
        relay.close().await;
    }
}
