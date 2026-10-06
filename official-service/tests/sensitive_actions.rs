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
        query("UPDATE web_sessions SET last_proof_at = NULL")
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

#[tokio::test]
async fn sensitive_actions_recheck_proof_and_session_after_waiting_for_locks() {
    for (method, suffix) in [
        (Method::POST, "/detach"),
        (Method::DELETE, ""),
        (Method::POST, "/methods/remove"),
    ] {
        for expire_session in [false, true] {
            let relay = RelayedInstallation::new(axum::Router::new()).await;
            let app = &relay.app;
            let id = relay.session["installations"][0]["id"].as_str().unwrap();
            let removing_method = suffix == "/methods/remove";
            let route = if removing_method {
                "/api/account/methods/remove".to_owned()
            } else {
                format!("/api/installations/{id}{suffix}")
            };
            let mut barrier = app.pool.begin().await.unwrap();
            let (barrier_pid,): (i32,) = sqlx_core::query_as::query_as("SELECT pg_backend_pid()")
                .fetch_one(&mut *barrier)
                .await
                .unwrap();
            let locked_table = if removing_method {
                "leo_accounts"
            } else {
                "installations"
            };
            query(&format!("SELECT id FROM {locked_table} FOR UPDATE"))
                .execute(&mut *barrier)
                .await
                .unwrap();
            let mut request =
                app.authenticated(&relay.cookie, &relay.session, method.clone(), &route);
            if removing_method {
                let methods: Value = app
                    .authenticated(
                        &relay.cookie,
                        &relay.session,
                        Method::GET,
                        "/api/account/methods",
                    )
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                request = request.json(&json!({ "id": methods["methods"][0]["id"] }));
            }
            let pending = tokio::spawn(async move { request.send().await.unwrap() });
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let (waiting,): (bool,) = sqlx_core::query_as::query_as(
                        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
                    ).bind(barrier_pid).fetch_one(&app.pool).await.unwrap();
                    if waiting { break; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.expect("the real HTTP action must reach the lock barrier");
            let expiry = if expire_session {
                "UPDATE web_sessions SET expires_at = clock_timestamp() - interval '1 millisecond'"
            } else {
                "UPDATE web_sessions SET last_proof_at = clock_timestamp() - interval '5 minutes'"
            };
            query(expiry).execute(&app.pool).await.unwrap();
            barrier.commit().await.unwrap();
            let response = pending.await.unwrap();
            let expected = if expire_session {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::FORBIDDEN
            };
            assert_eq!(response.status(), expected, "{route}");
            if !expire_session {
                assert_eq!(
                    relay.get("/chats").send().await.unwrap().status(),
                    StatusCode::OK
                );
            }
            relay.close().await;
        }
    }
}
