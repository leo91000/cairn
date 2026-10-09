mod common;

use common::{RelayedInstallation, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx_core::query::query;
use std::time::Duration;

#[tokio::test]
async fn removing_members_requires_proof_and_preserves_access_on_refusal() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let (member_cookie, member_session) = login(app, "member@example.test").await;
    let invitation: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/sharing/invitations"),
        )
        .json(&json!({ "email": "member@example.test" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let accepted = app
        .authenticated(
            &member_cookie,
            &member_session,
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
    let member = member_session["account"]["id"].as_str().unwrap();
    let route = format!("/api/installations/{id}/sharing/members/{member}");
    let mut stream = app
        .authenticated(
            &member_cookie,
            &member_session,
            Method::GET,
            &format!("/api/installations/{id}/api/chats/stream"),
        )
        .send()
        .await
        .unwrap();
    stream.chunk().await.unwrap().unwrap();
    query("UPDATE web_sessions SET last_proof_at = NULL WHERE account_id = $1")
        .bind(relay.session["account"]["id"].as_str().unwrap())
        .execute(&app.pool)
        .await
        .unwrap();

    let rejected = app
        .authenticated(&relay.cookie, &relay.session, Method::DELETE, &route)
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), async {
            while let Ok(Some(_)) = stream.chunk().await {}
        })
        .await
        .is_err(),
        "a refusal must preserve the member's live stream"
    );
    let readable = app
        .authenticated(
            &member_cookie,
            &member_session,
            Method::GET,
            &format!("/api/installations/{id}/api/chats"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(readable.status(), StatusCode::OK);

    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (confirmed_cookie, confirmed_session) = login(app, "relay-owner@example.test").await;
    let removed = app
        .authenticated(
            &confirmed_cookie,
            &confirmed_session,
            Method::DELETE,
            &route,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("confirmed removal must close the member's live stream");
    let denied = app
        .authenticated(
            &member_cookie,
            &member_session,
            Method::GET,
            &format!("/api/installations/{id}/api/chats"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn personal_tokens_require_proof_in_the_calling_session() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let route = format!("/api/installations/{id}/tokens");
    query("UPDATE web_sessions SET last_proof_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();
    let create = |cookie: &str, session: &Value| {
        app.authenticated(cookie, session, Method::POST, &route)
            .json(&json!({ "label": "Read client", "scopes": ["read"] }))
    };

    let rejected = create(&relay.cookie, &relay.session).send().await.unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    let grants: Value = app
        .authenticated(&relay.cookie, &relay.session, Method::GET, &route)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grants, json!([]));

    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (confirmed_cookie, confirmed_session) = login(app, "relay-owner@example.test").await;
    assert_eq!(
        create(&relay.cookie, &relay.session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let accepted = create(&confirmed_cookie, &confirmed_session)
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::CREATED);
    let granted: Value = accepted.json().await.unwrap();
    let call = app
        .client
        .post(format!("{}/mcp", app.url))
        .bearer_auth(granted["token"].as_str().unwrap())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_agents",
                "arguments": {},
            },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(call.status(), StatusCode::OK);
    relay.close().await;
}

#[tokio::test]
async fn revoking_other_sessions_requires_proof_but_self_logout_does_not() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (other_cookie, other_session) = login(app, "relay-owner@example.test").await;
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
    let individual = format!("/api/account/sessions/{}", other["id"].as_str().unwrap());
    query("UPDATE web_sessions SET last_proof_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();

    for (method, route) in [
        (Method::DELETE, individual.as_str()),
        (Method::POST, "/api/account/sessions/revoke-others"),
    ] {
        let rejected = app
            .authenticated(&relay.cookie, &relay.session, method, route)
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        let readable = app
            .authenticated(
                &other_cookie,
                &other_session,
                Method::GET,
                &format!("/api/installations/{id}/api/chats"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(readable.status(), StatusCode::OK);
    }

    let self_session = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["current"] == true)
        .unwrap();
    let logged_out = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!(
                "/api/account/sessions/{}",
                self_session["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(logged_out.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        app.authenticated(
            &other_cookie,
            &other_session,
            Method::GET,
            &format!("/api/installations/{id}/api/chats")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );

    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (confirmed_cookie, confirmed_session) = login(app, "relay-owner@example.test").await;
    let revoked = app
        .authenticated(
            &confirmed_cookie,
            &confirmed_session,
            Method::DELETE,
            &individual,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        app.authenticated(
            &other_cookie,
            &other_session,
            Method::GET,
            &format!("/api/installations/{id}/api/chats")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::UNAUTHORIZED
    );
    relay.close().await;
}

#[tokio::test]
async fn mcp_consent_requires_proof_for_approval_but_not_denial() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let client: Value = app
        .client
        .post(format!("{}/oauth/register", app.url))
        .json(&json!({
            "client_name": "Confirmed client",
            "redirect_uris": ["http://localhost:9999/callback"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let verifier = "a".repeat(43);
    let parameters = json!({
        "client_id": client["client_id"],
        "redirect_uri": "http://localhost:9999/callback",
        "response_type": "code",
        "code_challenge_method": "S256",
        "code_challenge": URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        "resource": format!("{}/mcp", app.url),
        "scope": "read",
        "state": "preserved-state",
    });
    let consent = |cookie: &str, session: &Value, approved: bool| {
        app.authenticated(cookie, session, Method::POST, "/api/mcp/oauth/consent")
            .json(&json!({
                "parameters": parameters,
                "installationId": id,
                "approved": approved,
            }))
    };
    query("UPDATE web_sessions SET last_proof_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();

    for _ in 0..31 {
        assert_eq!(
            consent(&relay.cookie, &relay.session, true)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let denied = consent(&relay.cookie, &relay.session, false)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::OK);
    let denial: Value = denied.json().await.unwrap();
    assert_eq!(
        denial["redirect"],
        "http://localhost:9999/callback?state=preserved-state&error=access_denied"
    );
    let grants: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::GET,
            &format!("/api/installations/{id}/tokens"),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grants, json!([]));

    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (confirmed_cookie, confirmed_session) = login(app, "relay-owner@example.test").await;
    let approved = consent(&confirmed_cookie, &confirmed_session, true)
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), StatusCode::OK);
    let approval: Value = approved.json().await.unwrap();
    let redirect = url::Url::parse(approval["redirect"].as_str().unwrap()).unwrap();
    let code = redirect
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .into_owned();
    let exchange = app
        .client
        .post(format!("{}/oauth/token", app.url))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client["client_id"].as_str().unwrap()),
            ("redirect_uri", "http://localhost:9999/callback"),
            ("code", &code),
            ("code_verifier", &verifier),
            ("resource", &format!("{}/mcp", app.url)),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(exchange.status(), StatusCode::OK);
    let token: Value = exchange.json().await.unwrap();
    let call = app
        .client
        .post(format!("{}/mcp", app.url))
        .bearer_auth(token["access_token"].as_str().unwrap())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(call.status(), StatusCode::OK);
    relay.close().await;
}

#[tokio::test]
async fn confirmed_token_creation_can_wait_for_access_without_deadlocking_oauth_exchange() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let client: Value = app
        .client
        .post(format!("{}/oauth/register", app.url))
        .json(&json!({
            "client_name": "Concurrent client",
            "redirect_uris": ["http://localhost:9999/callback"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let verifier = "a".repeat(43);
    let parameters = json!({
        "client_id": client["client_id"],
        "redirect_uri": "http://localhost:9999/callback",
        "response_type": "code",
        "code_challenge_method": "S256",
        "code_challenge": URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        "resource": format!("{}/mcp", app.url),
        "scope": "read",
    });
    let approval: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/mcp/oauth/consent",
        )
        .json(&json!({
            "parameters": parameters,
            "installationId": id,
            "approved": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let redirect = url::Url::parse(approval["redirect"].as_str().unwrap()).unwrap();
    let code = redirect
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .into_owned();

    // Hold the same installation read lock used by an in-flight OAuth exchange.
    let mut barrier = app.pool.begin().await.unwrap();
    let (barrier_pid,): (i32,) = sqlx_core::query_as::query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    query("SELECT id FROM installations WHERE id = $1 FOR SHARE")
        .bind(id)
        .execute(&mut *barrier)
        .await
        .unwrap();
    let request = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/tokens"),
        )
        .json(&json!({ "label": "Waiting personal client", "scopes": ["read"] }));
    let personal = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (waiting,): (bool,) = sqlx_core::query_as::query_as(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
            ).bind(barrier_pid).fetch_one(&app.pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("personal creation must wait behind the installation read lock");
    let exchange_request = app.client.post(format!("{}/oauth/token", app.url)).form(&[
        ("grant_type", "authorization_code"),
        ("client_id", client["client_id"].as_str().unwrap()),
        ("redirect_uri", "http://localhost:9999/callback"),
        ("code", &code),
        ("code_verifier", &verifier),
        ("resource", &format!("{}/mcp", app.url)),
    ]);
    let exchanged = tokio::time::timeout(Duration::from_secs(3), exchange_request.send()).await;

    // Release the barrier even on failure, so the test does not leave a pending request.
    barrier.commit().await.unwrap();
    let personal = personal.await.unwrap();
    assert_eq!(personal.status(), StatusCode::CREATED);
    let exchanged = exchanged
        .expect("OAuth exchange must finish while confirmed creation is waiting")
        .unwrap();
    assert_eq!(exchanged.status(), StatusCode::OK);
    relay.close().await;
}
