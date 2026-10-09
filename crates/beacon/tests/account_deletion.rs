mod common;

use common::{RelayedInstallation, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx_core::{query::query, query_as::query_as};
use std::time::Duration;

#[tokio::test]
async fn deleting_an_owner_detaches_sharing_and_preserves_data_for_a_new_claim() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sharing = format!("/api/installations/{id}/sharing");
    let chat: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/api/chats"),
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let (member_cookie, member_session) = login(app, "member@example.test").await;
    let invite: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{sharing}/invitations"),
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
                invite["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    let pending = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{sharing}/invitations"),
        )
        .json(&json!({ "email": "pending@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pending.status(), StatusCode::CREATED);
    let mut owner_stream = relay.get("/chats/stream").send().await.unwrap();
    owner_stream.chunk().await.unwrap().unwrap();
    let mut member_stream = app
        .client
        .get(format!("{}/chats/stream", relay.base))
        .header("cookie", &member_cookie)
        .send()
        .await
        .unwrap();
    member_stream.chunk().await.unwrap().unwrap();

    let mismatched = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "someone-else@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(mismatched.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let deleted = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(
        deleted.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    for stream in [&mut owner_stream, &mut member_stream] {
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Ok(Some(_)) = stream.chunk().await {}
        })
        .await
        .expect("account deletion must close all owned tunnels immediately");
    }
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.client
            .get(format!("{}/chats", relay.base))
            .header("cookie", &member_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let member: Value = app
        .authenticated(
            &member_cookie,
            &member_session,
            Method::GET,
            "/api/account/session",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(member["authenticated"], true);
    assert_eq!(member["installations"], json!([]));
    tokio::time::timeout(Duration::from_secs(3), &mut relay.connector)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let directory = relay
        .installation
        .config
        .data_dir
        .join("installation-relay");
    let (display, receipt) = tokio::sync::oneshot::channel();
    let claim_directory = directory.clone();
    let claim = tokio::spawn(async move {
        cairn_installation::relay::device_claim(
            None,
            &claim_directory,
            "Recovered",
            tokio_util::sync::CancellationToken::new(),
            move |_, code, _, _| {
                display.send(code.to_owned()).unwrap();
            },
        )
        .await
    });
    let user_code = receipt.await.unwrap();
    let (new_cookie, new_session) = login(app, "new-owner@example.test").await;
    let preview: Value = app
        .authenticated(
            &new_cookie,
            &new_session,
            Method::POST,
            "/api/installations/device-claim/preview",
        )
        .json(&json!({ "code": user_code }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let approved = app
        .authenticated(
            &new_cookie,
            &new_session,
            Method::POST,
            "/api/installations/device-claim",
        )
        .json(&json!({ "code": user_code, "confirmation": preview["confirmation"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(5), claim)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let router = cairn_installation::http::router(relay.installation.clone())
        .await
        .unwrap();
    relay.connector = tokio::spawn(cairn_installation::relay::connect(
        directory,
        router,
        relay.installation.clone(),
        relay.stop.clone(),
    ));
    relay.cookie = new_cookie;
    relay.session = new_session;
    let chats = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = relay.get("/chats").send().await.unwrap();
            if response.status() == StatusCode::OK {
                break response.json::<Value>().await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(chats[0]["id"], chat["id"]);
    let sharing: Value = relay
        .app
        .authenticated(&relay.cookie, &relay.session, Method::GET, &sharing)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sharing["members"], json!([]));
    assert_eq!(sharing["invitations"], json!([]));
    drop(owner_stream);
    drop(member_stream);
    relay.close().await;
}

#[tokio::test]
async fn deleting_a_member_preserves_the_owners_installation_and_requires_current_authorization() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let sharing = format!("/api/installations/{id}/sharing");
    let (cookie, session) = login(app, "departing@example.test").await;
    let invite: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{sharing}/invitations"),
        )
        .json(&json!({ "email": "departing@example.test" }))
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
                invite["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    for (origin, csrf, expected) in [
        (&app.url[..], "wrong", StatusCode::FORBIDDEN),
        (
            "https://foreign.example",
            session["csrf"].as_str().unwrap(),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let rejected = app
            .client
            .post(format!("{}/api/account/delete", app.url))
            .header("origin", origin)
            .header("cookie", &cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({ "email": "departing@example.test" }))
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), expected);
    }
    let mut stream = app
        .client
        .get(format!("{}/chats/stream", relay.base))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    stream.chunk().await.unwrap().unwrap();
    let deleted = app
        .authenticated(&cookie, &session, Method::POST, "/api/account/delete")
        .json(&json!({ "email": "departing@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("deleting a member must close only their live access");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let shared: Value = app
        .authenticated(&relay.cookie, &relay.session, Method::GET, &sharing)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shared["members"], json!([]));
    let replayed = app
        .authenticated(&cookie, &session, Method::POST, "/api/account/delete")
        .json(&json!({ "email": "departing@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(replayed.status(), StatusCode::UNAUTHORIZED);
    drop(stream);
    relay.close().await;
}

// Fixture locks only coordinate HTTP callers; verdicts stay at the beacon API.
async fn wait_for_account_lock(app: &common::Fixture, account: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let probe = query("SELECT id FROM cairn_accounts WHERE id = $1 FOR UPDATE NOWAIT")
                .bind(account)
                .execute(&app.pool)
                .await;
            if let Err(error) = probe {
                assert_eq!(
                    error.as_database_error().unwrap().code().as_deref(),
                    Some("55P03")
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the deletion must reach the account lock before the fixture releases installations");
}

async fn share_with(
    app: &common::Fixture,
    owner_cookie: &str,
    owner: &Value,
    installation: &str,
    member_cookie: &str,
    member: &Value,
) {
    let invitation: Value = app
        .authenticated(
            owner_cookie,
            owner,
            Method::POST,
            &format!("/api/installations/{installation}/sharing/invitations"),
        )
        .json(&json!({ "email": member["account"]["email"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let accepted = app
        .authenticated(
            member_cookie,
            member,
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
}

#[tokio::test]
async fn mutually_shared_owners_can_delete_their_accounts_concurrently() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let first_id = relay.session["installations"][0]["id"].as_str().unwrap();
    let (cookie, session) = login(app, "second-owner@example.test").await;
    let code: Value = app
        .authenticated(
            &cookie,
            &session,
            Method::POST,
            "/api/installations/claim-code",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let claimed: Value = app
        .post(
            "/api/relay/claim",
            json!({
                "code": code["code"],
                "name": "Second installation",
                "protocol": 1,
            }),
        )
        .await
        .json()
        .await
        .unwrap();
    let second_id = claimed["installationId"].as_str().unwrap();
    share_with(
        app,
        &relay.cookie,
        &relay.session,
        first_id,
        &cookie,
        &session,
    )
    .await;
    share_with(
        app,
        &cookie,
        &session,
        second_id,
        &relay.cookie,
        &relay.session,
    )
    .await;

    let mut barrier = app.pool.begin().await.unwrap();
    query("SELECT id FROM installations ORDER BY id FOR UPDATE")
        .execute(&mut *barrier)
        .await
        .unwrap();
    let first = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }));
    let second = app
        .authenticated(&cookie, &session, Method::POST, "/api/account/delete")
        .json(&json!({ "email": "second-owner@example.test" }));
    let first = tokio::spawn(async move { first.send().await.unwrap() });
    let second = tokio::spawn(async move { second.send().await.unwrap() });
    wait_for_account_lock(app, relay.session["account"]["id"].as_str().unwrap()).await;
    wait_for_account_lock(app, session["account"]["id"].as_str().unwrap()).await;
    barrier.commit().await.unwrap();

    let first = first.await.unwrap();
    let second = second.await.unwrap();
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    assert_eq!(second.status(), StatusCode::NO_CONTENT);
    for (cookie, session) in [(&relay.cookie, &relay.session), (&cookie, &session)] {
        let status: Value = app
            .authenticated(cookie, session, Method::GET, "/api/account/session")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["authenticated"], false);
    }
    relay.close().await;
}

async fn wait_for_blocked_caller(app: &common::Fixture, blocker: i32) -> i32 {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: Option<(i32,)> = query_as(
                "SELECT pid FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) LIMIT 1",
            )
            .bind(blocker)
            .fetch_optional(&app.admin)
            .await
            .unwrap();
            if let Some((pid,)) = waiting {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the HTTP caller must reach the fixture's lock barrier")
}

#[tokio::test]
async fn deletion_invalidates_a_pending_email_verification_without_deadlocking() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    // Advance the synthetic delivery window after the setup login.
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let challenge = app
        .post(
            "/api/account/email-code",
            json!({
                "email": "relay-owner@example.test",
            }),
        )
        .await;
    assert_eq!(challenge.status(), StatusCode::ACCEPTED);
    let challenge: Value = challenge.json().await.unwrap();
    let code = app.mail.0.lock().unwrap().last().unwrap().1.clone();

    // Stop deletion after its account lock, then admit the competing verifier.
    let mut barrier = app.pool.begin().await.unwrap();
    let (barrier_pid,): (i32,) = query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    query("SELECT id FROM installations ORDER BY id FOR UPDATE")
        .execute(&mut *barrier)
        .await
        .unwrap();
    let deletion = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }));
    let deletion = tokio::spawn(async move { deletion.send().await.unwrap() });
    let deleting_pid = wait_for_blocked_caller(app, barrier_pid).await;
    let verification = app
        .client
        .post(format!("{}/api/account/verify", app.url))
        .header("origin", &app.url)
        .json(&json!({ "challenge": challenge["challenge"], "code": code }));
    let verification = tokio::spawn(async move { verification.send().await.unwrap() });
    wait_for_blocked_caller(app, deleting_pid).await;
    barrier.commit().await.unwrap();

    let deletion = deletion.await.unwrap();
    let verification = verification.await.unwrap();
    assert_eq!(deletion.status(), StatusCode::NO_CONTENT);
    assert_eq!(verification.status(), StatusCode::UNAUTHORIZED);
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
    assert_eq!(session["authenticated"], false);
    relay.close().await;
}

#[tokio::test]
async fn a_session_expiring_while_deletion_waits_cannot_delete_the_account() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let mut barrier = app.pool.begin().await.unwrap();
    let (barrier_pid,): (i32,) = query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    query("SELECT id FROM cairn_accounts WHERE id = $1 FOR UPDATE")
        .bind(relay.session["account"]["id"].as_str().unwrap())
        .execute(&mut *barrier)
        .await
        .unwrap();
    let deletion = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }));
    let deletion = tokio::spawn(async move { deletion.send().await.unwrap() });
    wait_for_blocked_caller(app, barrier_pid).await;
    // Expire after the deleting transaction began, before revalidation executes.
    tokio::time::sleep(Duration::from_millis(20)).await;
    query("UPDATE web_sessions SET expires_at = clock_timestamp() - interval '1 millisecond'")
        .execute(&app.pool)
        .await
        .unwrap();
    barrier.commit().await.unwrap();
    assert_eq!(deletion.await.unwrap().status(), StatusCode::UNAUTHORIZED);

    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let (_, session) = login(app, "relay-owner@example.test").await;
    assert_eq!(session["account"]["id"], relay.session["account"]["id"]);
    assert_eq!(session["installations"][0]["role"], "owner");
    relay.close().await;
}

#[tokio::test]
async fn deletion_retries_rolled_back_database_deadlocks_and_bounds_persistent_failures() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    // PostgreSQL sequences survive rollback, so the fixture can fail whole
    // deletion transactions twice without relying on nondeterministic victims.
    sqlx_core::raw_sql::raw_sql("CREATE SEQUENCE deletion_attempts; CREATE FUNCTION fail_deletion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('deletion_attempts') <= 2 THEN RAISE EXCEPTION 'fixture deadlock' USING ERRCODE = '40P01'; END IF; RETURN OLD; END $$; CREATE TRIGGER deletion_deadlock BEFORE DELETE ON cairn_accounts FOR EACH ROW EXECUTE FUNCTION fail_deletion();")
        .execute(&app.pool).await.unwrap();
    let deletion = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deletion.status(), StatusCode::NO_CONTENT);
    let status: Value = app
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
    assert_eq!(status["authenticated"], false);
    relay.close().await;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    sqlx_core::raw_sql::raw_sql("CREATE FUNCTION fail_deletion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture deadlock' USING ERRCODE = '40P01'; END $$; CREATE TRIGGER deletion_deadlock BEFORE DELETE ON cairn_accounts FOR EACH ROW EXECUTE FUNCTION fail_deletion();")
        .execute(&app.pool).await.unwrap();
    let deletion = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }));
    let deletion = tokio::time::timeout(Duration::from_secs(3), deletion.send())
        .await
        .expect("permanent contention must stop after a bounded number of attempts")
        .unwrap();
    assert_eq!(deletion.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let status: Value = app
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
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["installations"][0]["role"], "owner");
    relay.close().await;
}

#[tokio::test]
async fn deletion_requires_a_recent_email_or_passkey_proof_in_the_callers_session() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    query("UPDATE web_sessions SET last_proof_at = now() - interval '1 hour'")
        .execute(&app.pool)
        .await
        .unwrap();

    let rejected = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn email_confirmation_is_single_use_for_only_the_current_account_and_session() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (other_cookie, other_session) = login(app, "relay-owner@example.test").await;
    let (stranger_cookie, stranger) = login(app, "stranger@example.test").await;
    query("UPDATE web_sessions SET last_proof_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
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
    let proof = json!({ "challenge": challenge["challenge"], "code": code });
    let foreign = app
        .authenticated(
            &stranger_cookie,
            &stranger,
            Method::POST,
            "/api/account/reauth/email",
        )
        .json(&proof)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::UNAUTHORIZED);
    let wrong_csrf = app
        .client
        .post(format!("{}/api/account/reauth/email", app.url))
        .header("origin", &app.url)
        .header("cookie", &relay.cookie)
        .json(&proof)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_csrf.status(), StatusCode::FORBIDDEN);
    let confirmed = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/reauth/email",
        )
        .json(&proof)
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::NO_CONTENT);
    assert!(!confirmed.headers().contains_key("set-cookie"));
    let replay = app
        .authenticated(
            &other_cookie,
            &other_session,
            Method::POST,
            "/api/account/reauth/email",
        )
        .json(&proof)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    let other_delete = app
        .authenticated(
            &other_cookie,
            &other_session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(other_delete.status(), StatusCode::FORBIDDEN);
    let deleted = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        app.authenticated(
            &other_cookie,
            &other_session,
            Method::GET,
            "/api/account/sessions"
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
async fn deletion_revokes_other_sessions_mcp_token_families_and_inbound_invitations() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (other_cookie, other_session) = login(app, "relay-owner@example.test").await;
    let personal: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/tokens"),
        )
        .json(&json!({ "label": "Personal", "scopes": ["read"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client: Value = app
        .post(
            "/oauth/register",
            json!({
                "client_name": "Deletion test",
                "redirect_uris": ["http://localhost:9999/callback"],
                "token_endpoint_auth_method": "none",
            }),
        )
        .await
        .json()
        .await
        .unwrap();
    let verifier = "v".repeat(43);
    let consent: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/mcp/oauth/consent",
        )
        .json(&json!({
            "parameters": {
                "client_id": client["client_id"],
                "redirect_uri": "http://localhost:9999/callback",
                "response_type": "code",
                "code_challenge_method": "S256",
                "code_challenge": URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
                "scope": "read",
            },
            "installationId": id,
            "approved": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let callback = url::Url::parse(consent["redirect"].as_str().unwrap()).unwrap();
    let code = callback
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .to_string();
    let tokens: Value = app
        .client
        .post(format!("{}/oauth/token", app.url))
        .form(&json!({
            "grant_type": "authorization_code",
            "client_id": client["client_id"],
            "redirect_uri": "http://localhost:9999/callback",
            "code": code,
            "code_verifier": verifier,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let call = |token: &Value| {
        app.client
            .post(format!("{}/mcp", app.url))
            .bearer_auth(token.as_str().unwrap())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
            }))
    };
    for token in [&personal["token"], &tokens["access_token"]] {
        assert_eq!(call(token).send().await.unwrap().status(), StatusCode::OK);
    }

    let (inviter_cookie, inviter) = login(app, "inviter@example.test").await;
    let claim: Value = app
        .authenticated(
            &inviter_cookie,
            &inviter,
            Method::POST,
            "/api/installations/claim-code",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let installation: Value = app
        .post(
            "/api/relay/claim",
            json!({
                "code": claim["code"],
                "name": "Invitation source",
                "protocol": 1,
            }),
        )
        .await
        .json()
        .await
        .unwrap();
    let invitation_route = format!(
        "/api/installations/{}/sharing/invitations",
        installation["installationId"].as_str().unwrap()
    );
    let invitation = app
        .authenticated(&inviter_cookie, &inviter, Method::POST, &invitation_route)
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(invitation.status(), StatusCode::CREATED);
    let pending: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::GET,
            "/api/account/invitations",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);

    let deleted = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        app.authenticated(
            &other_cookie,
            &other_session,
            Method::GET,
            "/api/account/sessions"
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::UNAUTHORIZED
    );
    for token in [&personal["token"], &tokens["access_token"]] {
        assert_eq!(
            call(token).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let refresh = app
        .client
        .post(format!("{}/oauth/token", app.url))
        .form(&json!({
            "grant_type": "refresh_token",
            "client_id": client["client_id"],
            "refresh_token": tokens["refresh_token"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::BAD_REQUEST);
    let sharing: Value = app
        .authenticated(
            &inviter_cookie,
            &inviter,
            Method::GET,
            &format!(
                "/api/installations/{}/sharing",
                installation["installationId"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sharing["invitations"], json!([]));
    relay.close().await;
}

#[tokio::test]
async fn a_proof_expiring_while_deletion_waits_does_not_authorize_the_mutation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let mut barrier = app.pool.begin().await.unwrap();
    query("SELECT id FROM installations FOR UPDATE")
        .execute(&mut *barrier)
        .await
        .unwrap();
    let request = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }));
    let deletion = tokio::spawn(async move { request.send().await.unwrap() });
    wait_for_account_lock(app, relay.session["account"]["id"].as_str().unwrap()).await;
    query("UPDATE web_sessions SET last_proof_at = clock_timestamp() - interval '5 minutes'")
        .execute(&app.pool)
        .await
        .unwrap();
    barrier.commit().await.unwrap();
    assert_eq!(deletion.await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn an_unconfirmed_deletion_neither_waits_for_account_locks_nor_spends_the_deletion_budget() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    query("UPDATE web_sessions SET last_proof_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();
    let mut barrier = app.pool.begin().await.unwrap();
    query("SELECT id FROM cairn_accounts FOR UPDATE")
        .execute(&mut *barrier)
        .await
        .unwrap();
    for _ in 0..6 {
        let request = app
            .authenticated(
                &relay.cookie,
                &relay.session,
                Method::POST,
                "/api/account/delete",
            )
            .json(&json!({ "email": "relay-owner@example.test" }));
        let response = tokio::time::timeout(Duration::from_secs(1), request.send())
            .await
            .expect("a missing proof must be refused before waiting for locks")
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    barrier.commit().await.unwrap();
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
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
    let confirmation = app
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
    assert_eq!(confirmation.status(), StatusCode::NO_CONTENT);
    let deletion = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deletion.status(), StatusCode::NO_CONTENT);
    relay.close().await;
}
