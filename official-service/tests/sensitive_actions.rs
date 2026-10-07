mod common;

use common::{RelayedInstallation, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx_core::query::query;
use std::time::Duration;

#[tokio::test]
async fn invitations_require_recent_proof_before_sending_mail_or_granting_access() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    query("UPDATE web_sessions SET last_proof_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();

    // Refusals without proof must not spend the invitation budget.
    for _ in 0..11 {
        let rejected = app
            .authenticated(
                &relay.cookie,
                &relay.session,
                Method::POST,
                &format!("/api/installations/{id}/sharing/invitations"),
            )
            .json(&json!({ "email": "unconfirmed-invite@example.test" }))
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    }

    assert!(app.mail.1.lock().unwrap().is_empty());
    let sharing: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::GET,
            &format!("/api/installations/{id}/sharing"),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sharing["invitations"], json!([]));
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );

    confirm_owner(&relay).await;
    let invited = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/sharing/invitations"),
        )
        .json(&json!({ "email": "unconfirmed-invite@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(invited.status(), StatusCode::CREATED);
    assert_eq!(app.mail.1.lock().unwrap().len(), 1);
    relay.close().await;
}

async fn confirm_owner(relay: &RelayedInstallation) {
    let app = &relay.app;
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
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
}

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
        (Method::POST, "/api/account/methods/remove"),
        (Method::POST, "/sharing/invitations"),
        (Method::DELETE, "/sharing/members"),
        (Method::POST, "/tokens"),
        (Method::POST, "/api/mcp/oauth/consent"),
        (Method::POST, "/api/account/sessions/revoke-others"),
        (Method::DELETE, "/api/account/sessions/other"),
    ] {
        for expire_session in [false, true] {
            let relay = RelayedInstallation::new(axum::Router::new()).await;
            let app = &relay.app;
            let id = relay.session["installations"][0]["id"].as_str().unwrap();
            let account_action = suffix.starts_with("/api/account/");
            let mut route = if suffix.starts_with("/api/") {
                suffix.to_owned()
            } else {
                format!("/api/installations/{id}{suffix}")
            };
            let body = match suffix {
                "/api/account/methods/remove" => {
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
                    Some(json!({ "id": methods["methods"][0]["id"] }))
                }
                "/sharing/invitations" => Some(json!({ "email": "blocked-invite@example.test" })),
                "/tokens" => Some(json!({ "label": "Blocked client", "scopes": ["read"] })),
                "/sharing/members" => {
                    let (cookie, session) = login(app, "blocked-member@example.test").await;
                    let invitation: Value = app
                        .authenticated(
                            &relay.cookie,
                            &relay.session,
                            Method::POST,
                            &format!("/api/installations/{id}/sharing/invitations"),
                        )
                        .json(&json!({ "email": "blocked-member@example.test" }))
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    let accepted = app
                        .authenticated(
                            &cookie,
                            &session,
                            Method::POST,
                            &format!(
                                "/api/account/invitations/{}/accept",
                                invitation["id"].as_str().unwrap()
                            ),
                        )
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
                    route.push('/');
                    route.push_str(session["account"]["id"].as_str().unwrap());
                    None
                }
                "/api/mcp/oauth/consent" => {
                    let client: Value = app
                        .client
                        .post(format!("{}/oauth/register", app.url))
                        .json(&json!({
                            "client_name": "Blocked client",
                            "redirect_uris": ["http://localhost:9999/callback"],
                        }))
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    Some(json!({
                        "parameters": {
                            "client_id": client["client_id"],
                            "redirect_uri": "http://localhost:9999/callback",
                            "response_type": "code",
                            "code_challenge_method": "S256",
                            "code_challenge": "a".repeat(43),
                            "resource": format!("{}/mcp", app.url),
                            "scope": "read",
                        },
                        "installationId": id,
                        "approved": true,
                    }))
                }
                "/api/account/sessions/other" => {
                    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
                        .execute(&app.pool).await.unwrap();
                    let _ = login(app, "relay-owner@example.test").await;
                    let sessions: Value = app
                        .authenticated(
                            &relay.cookie,
                            &relay.session,
                            Method::GET,
                            "/api/account/sessions",
                        )
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    let other = sessions["sessions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|session| session["current"] == false)
                        .unwrap();
                    route = format!("/api/account/sessions/{}", other["id"].as_str().unwrap());
                    None
                }
                _ => None,
            };

            let mut barrier = app.pool.begin().await.unwrap();
            let (barrier_pid,): (i32,) = sqlx_core::query_as::query_as("SELECT pg_backend_pid()")
                .fetch_one(&mut *barrier)
                .await
                .unwrap();
            let locked_table = if account_action {
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
            if let Some(body) = body {
                request = request.json(&body);
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
                "UPDATE web_sessions SET expires_at = clock_timestamp() - interval '1 millisecond' WHERE csrf = $1"
            } else {
                "UPDATE web_sessions SET last_proof_at = clock_timestamp() - interval '5 minutes' WHERE csrf = $1"
            };
            query(expiry)
                .bind(relay.session["csrf"].as_str().unwrap())
                .execute(&app.pool)
                .await
                .unwrap();
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
