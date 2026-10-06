mod common;

use common::{RelayedInstallation, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn access_changes_have_a_private_transactional_audit_without_email_or_credentials() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let response = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::GET,
            "/api/account/audit",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let initial: Value = response.json().await.unwrap();
    assert_eq!(initial["events"][0]["action"], "installation.claimed");
    assert_eq!(initial["events"][0]["installationId"], id);
    let sharing = format!("/api/installations/{id}/sharing");
    let invitation: Value = app
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
    let (cookie, session) = login(app, "member@example.test").await;
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
    let member_id = session["account"]["id"].as_str().unwrap();
    let removed = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!("{sharing}/members/{member_id}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    let rejected = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!("{sharing}/members/{member_id}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::NOT_FOUND);
    let detached = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/detach"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(detached.status(), StatusCode::NO_CONTENT);
    let audit: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::GET,
            "/api/account/audit",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let events = audit["events"].as_array().unwrap();
    let actions: Vec<&str> = events
        .iter()
        .map(|event| event["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        [
            "installation.detached",
            "member.removed",
            "invitation.accepted",
            "invitation.created",
            "installation.claimed"
        ]
    );
    assert_eq!(events[1]["targetId"], member_id);
    let serialized = serde_json::to_string(&audit).unwrap();
    for secret in [
        "member@example.test",
        "relay-owner@example.test",
        "digest",
        relay.session["csrf"].as_str().unwrap(),
        relay.cookie.split('=').nth(1).unwrap(),
    ] {
        assert!(
            !serialized.contains(secret),
            "audit must contain only access metadata"
        );
    }
    let (stranger_cookie, stranger_session) = login(app, "stranger@example.test").await;
    let foreign: Value = app
        .authenticated(
            &stranger_cookie,
            &stranger_session,
            Method::GET,
            "/api/account/audit",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(foreign["events"], json!([]));
    let anonymous = app
        .client
        .get(format!("{}/api/account/audit", app.url))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    relay.close().await;
}

#[tokio::test]
async fn a_member_can_read_their_own_access_history_after_leaving() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let (cookie, session) = login(app, "member@example.test").await;
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
    let left = app
        .authenticated(
            &cookie,
            &session,
            Method::DELETE,
            &format!("/api/installations/{id}/sharing/membership"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(left.status(), StatusCode::NO_CONTENT);
    let audit: Value = app
        .authenticated(&cookie, &session, Method::GET, "/api/account/audit")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let actions: Vec<&str> = audit["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["member.left", "invitation.accepted"]);
    relay.close().await;
}

#[tokio::test]
async fn audit_failure_rolls_back_access_changes_and_retention_is_enforced_without_cleanup() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    // Inject a write failure at the external Postgres boundary. Observe only
    // official HTTP sharing/audit responses, not persisted implementation state.
    sqlx_core::query::query("ALTER TABLE account_audit ADD CONSTRAINT fixture_failure CHECK (action <> 'invitation.created')")
        .execute(&app.pool).await.unwrap();
    let rejected = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/sharing/invitations"),
        )
        .json(&json!({ "email": "member@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::INTERNAL_SERVER_ERROR);
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
    let audit: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::GET,
            "/api/account/audit",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(audit["events"].as_array().unwrap().len(), 1);
    sqlx_core::query::query("UPDATE account_audit SET created_at = now() - interval '91 days'")
        .execute(&app.pool)
        .await
        .unwrap();
    for clean in [false, true] {
        if clean {
            leo_official_service::cleanup_expired(&app.pool)
                .await
                .unwrap();
        }
        let expired: Value = app
            .authenticated(
                &relay.cookie,
                &relay.session,
                Method::GET,
                "/api/account/audit",
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(expired["events"], json!([]));
    }
    relay.close().await;
}
